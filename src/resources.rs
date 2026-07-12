//! Pure builders mapping a `CalibanTask` to the child objects a reconcile
//! applies: a token-less ServiceAccount, a default-deny NetworkPolicy, and the
//! backing agent-sandbox Sandbox. No cluster access — unit-tested. See ADR 0002.

use std::collections::BTreeMap;

use k8s_openapi::api::core::v1::{
    Container, ContainerPort, EnvVar, EnvVarSource, PersistentVolumeClaimSpec, PodSpec,
    PodTemplateSpec, SecretKeySelector, ServiceAccount, VolumeMount, VolumeResourceRequirements,
};
use k8s_openapi::api::networking::v1::{
    NetworkPolicy, NetworkPolicyEgressRule, NetworkPolicyIngressRule, NetworkPolicyPeer,
    NetworkPolicyPort, NetworkPolicySpec,
};
use k8s_openapi::apimachinery::pkg::api::resource::Quantity;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{LabelSelector, ObjectMeta};
use k8s_openapi::apimachinery::pkg::util::intstr::IntOrString;
use kube::ResourceExt;

use crate::config::{common_labels, netpol_name, owner_ref, sa_name, sandbox_name, Settings};
use crate::crd::CalibanTask;
use crate::sandbox::{Sandbox, SandboxSpec, VolumeClaimTemplate};
use crate::workspace::ResolvedProvider;

fn child_meta(t: &CalibanTask, name: String, labels: BTreeMap<String, String>) -> ObjectMeta {
    ObjectMeta {
        name: Some(name),
        namespace: t.namespace(),
        labels: Some(labels),
        owner_references: Some(vec![owner_ref(t)]),
        ..Default::default()
    }
}

/// A dedicated, token-less ServiceAccount — the pod's least-privilege identity.
pub fn build_service_account(t: &CalibanTask) -> ServiceAccount {
    ServiceAccount {
        metadata: child_meta(t, sa_name(t), common_labels(t)),
        automount_service_account_token: Some(false),
        ..Default::default()
    }
}

fn np_port(proto: &str, port: i32) -> NetworkPolicyPort {
    NetworkPolicyPort {
        protocol: Some(proto.to_string()),
        port: Some(IntOrString::Int(port)),
        ..Default::default()
    }
}

/// Default-deny NetworkPolicy: allow DNS + general egress + caliband-port ingress.
pub fn build_network_policy(t: &CalibanTask, s: &Settings) -> NetworkPolicy {
    NetworkPolicy {
        metadata: child_meta(t, netpol_name(t), common_labels(t)),
        spec: Some(NetworkPolicySpec {
            // Select the sandbox pod by the labels we propagate into its template.
            pod_selector: Some(LabelSelector {
                match_labels: Some(common_labels(t)),
                ..Default::default()
            }),
            policy_types: Some(vec!["Ingress".to_string(), "Egress".to_string()]),
            // Ingress: caliband port only.
            ingress: Some(vec![NetworkPolicyIngressRule {
                ports: Some(vec![np_port("TCP", s.caliband_port)]),
                from: Some(vec![NetworkPolicyPeer {
                    pod_selector: Some(LabelSelector::default()),
                    ..Default::default()
                }]),
            }]),
            // Egress: DNS (53 UDP+TCP), then everything else (git/providers).
            egress: Some(vec![
                NetworkPolicyEgressRule {
                    ports: Some(vec![np_port("UDP", 53), np_port("TCP", 53)]),
                    ..Default::default()
                },
                NetworkPolicyEgressRule::default(),
            ]),
        }),
    }
}

const WORKSPACE_VOLUME: &str = "workspace";

fn env(name: &str, value: String) -> EnvVar {
    EnvVar {
        name: name.to_string(),
        value: Some(value),
        ..Default::default()
    }
}

fn caliband_env(t: &CalibanTask) -> Vec<EnvVar> {
    let mut e = vec![];
    if let Some(ep) = t
        .spec
        .state
        .as_ref()
        .and_then(|st| st.gonzalo_endpoint.clone())
    {
        e.push(env("GONZALO_ENDPOINT", ep));
    }
    if let Some(r) = t
        .spec
        .model
        .as_ref()
        .and_then(|m| m.router_config_ref.clone())
    {
        e.push(env("CALIBAN_ROUTER_CONFIG_REF", r));
    }
    e
}

/// Project a resolved provider to caliband container env. Credentials reach the
/// pod via `secretKeyRef` (the operator never inlines the value).
#[allow(dead_code)]
pub(crate) fn provider_env(rp: &ResolvedProvider) -> Vec<EnvVar> {
    let mut e = vec![env("CALIBAN_PROVIDER", rp.kind.clone())];
    if let Some(u) = &rp.base_url {
        e.push(env("CALIBAN_PROVIDER_BASE_URL", u.clone()));
    }
    if let Some(m) = &rp.model {
        e.push(env("CALIBAN_MODEL", m.clone()));
    }
    if let Some(c) = &rp.credentials_ref {
        e.push(EnvVar {
            name: "CALIBAN_API_KEY".to_string(),
            value: None,
            value_from: Some(EnvVarSource {
                secret_key_ref: Some(SecretKeySelector {
                    name: c.secret_name.clone(),
                    key: c.key.clone(),
                    optional: Some(false),
                }),
                ..Default::default()
            }),
        });
    }
    e
}

/// POSIX single-quote a value for safe interpolation into `/bin/sh -c`.
fn sh_squote(v: &str) -> String {
    format!("'{}'", v.replace('\'', "'\\''"))
}

/// Build the idempotent clone script for the init container: for each workspace
/// source, clone `repo` at `ref` into `path` unless it's already a git checkout
/// (so pause/resume and pod restarts over the persistent PVC don't refetch).
fn clone_script(t: &CalibanTask) -> String {
    let mut s = String::from("set -eu\n");
    for src in &t.spec.workspace.sources {
        s.push_str(&format!(
            "if [ ! -d {git} ]; then git clone --depth 1 --branch {r} {repo} {path}; fi\n",
            git = sh_squote(&format!("{}/.git", src.path)),
            r = sh_squote(&src.r#ref),
            repo = sh_squote(&src.repo),
            path = sh_squote(&src.path),
        ));
    }
    s
}

/// The git-clone init container that populates the workspace volume from
/// `spec.workspace.sources[]`, if any are configured.
fn clone_init_container(t: &CalibanTask, s: &Settings) -> Option<Container> {
    if t.spec.workspace.sources.is_empty() {
        return None;
    }
    Some(Container {
        name: "clone-workspace".to_string(),
        image: Some(s.git_image.clone()),
        command: Some(vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            clone_script(t),
        ]),
        volume_mounts: Some(vec![VolumeMount {
            name: WORKSPACE_VOLUME.to_string(),
            mount_path: s.workspace_root.clone(),
            ..Default::default()
        }]),
        ..Default::default()
    })
}

fn workspace_pvc(s: &Settings) -> VolumeClaimTemplate {
    VolumeClaimTemplate {
        metadata: ObjectMeta {
            name: Some(WORKSPACE_VOLUME.to_string()),
            ..Default::default()
        },
        spec: Some(PersistentVolumeClaimSpec {
            access_modes: Some(vec!["ReadWriteOnce".to_string()]),
            resources: Some(VolumeResourceRequirements {
                requests: Some(BTreeMap::from([(
                    "storage".to_string(),
                    Quantity(s.workspace_storage.clone()),
                )])),
                ..Default::default()
            }),
            // storageClassName unset → cluster default (cluster-agnostic).
            ..Default::default()
        }),
    }
}

/// Map a `CalibanTask` to its backing agent-sandbox `Sandbox`.
pub fn build_sandbox(t: &CalibanTask, s: &Settings) -> Sandbox {
    let labels = common_labels(t);
    let container = Container {
        name: "caliband".to_string(),
        image: Some(s.caliband_image.clone()),
        ports: Some(vec![ContainerPort {
            container_port: s.caliband_port,
            name: Some("caliband".to_string()),
            ..Default::default()
        }]),
        args: Some(vec![
            "--workspace-root".to_string(),
            s.workspace_root.clone(),
            "--listen".to_string(),
            format!("0.0.0.0:{}", s.caliband_port),
        ]),
        env: Some(caliband_env(t)),
        volume_mounts: Some(vec![VolumeMount {
            name: WORKSPACE_VOLUME.to_string(),
            mount_path: s.workspace_root.clone(),
            ..Default::default()
        }]),
        ..Default::default()
    };
    let pod_spec = PodSpec {
        containers: vec![container],
        init_containers: clone_init_container(t, s).map(|c| vec![c]),
        runtime_class_name: t
            .spec
            .isolation
            .as_ref()
            .and_then(|i| i.runtime_class.clone()),
        service_account_name: Some(sa_name(t)),
        automount_service_account_token: Some(false),
        ..Default::default()
    };
    let mut sb = Sandbox::new(
        &sandbox_name(t),
        SandboxSpec {
            pod_template: PodTemplateSpec {
                metadata: Some(ObjectMeta {
                    labels: Some(labels.clone()),
                    ..Default::default()
                }),
                spec: Some(pod_spec),
            },
            service: Some(true),
            operating_mode: Some("Running".to_string()),
            volume_claim_templates: Some(vec![workspace_pvc(s)]),
        },
    );
    sb.metadata.namespace = t.namespace();
    sb.metadata.labels = Some(labels);
    sb.metadata.owner_references = Some(vec![owner_ref(t)]);
    sb
}

/// The child objects a single reconcile applies.
pub struct ReconcilePlan {
    /// The task's dedicated, token-less ServiceAccount.
    pub service_account: ServiceAccount,
    /// The default-deny NetworkPolicy scoping the sandbox pod's traffic.
    pub network_policy: NetworkPolicy,
    /// The backing agent-sandbox Sandbox.
    pub sandbox: Sandbox,
}

/// Assemble every child object for a task (pure).
pub fn plan(t: &CalibanTask, s: &Settings) -> ReconcilePlan {
    ReconcilePlan {
        service_account: build_service_account(t),
        network_policy: build_network_policy(t, s),
        sandbox: build_sandbox(t, s),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crd::{CalibanTaskSpec, Source, TaskSpec, Workspace};

    fn task() -> CalibanTask {
        let mut t = CalibanTask::new(
            "refactor-auth",
            CalibanTaskSpec {
                workspace: Workspace {
                    sources: vec![Source {
                        name: "caliban".into(),
                        repo: "git@x:caliban".into(),
                        r#ref: "main".into(),
                        path: "/work/caliban".into(),
                    }],
                    services: vec![],
                },
                task: TaskSpec {
                    prompt: "hi".into(),
                    agent_type: None,
                },
                model: None,
                state: None,
                isolation: None,
                resources: None,
                lifecycle: None,
            },
        );
        t.metadata.namespace = Some("team-a".into());
        t.metadata.uid = Some("uid-123".into());
        t
    }

    #[test]
    fn service_account_is_token_less_and_owned() {
        let sa = build_service_account(&task());
        assert_eq!(sa.metadata.name.as_deref(), Some("refactor-auth-sa"));
        assert_eq!(sa.metadata.namespace.as_deref(), Some("team-a"));
        assert_eq!(sa.automount_service_account_token, Some(false));
        let owners = sa.metadata.owner_references.unwrap();
        assert_eq!(owners[0].controller, Some(true));
        assert_eq!(owners[0].kind, "CalibanTask");
    }

    #[test]
    fn network_policy_is_default_deny_with_dns_and_caliband_ingress() {
        let np = build_network_policy(&task(), &Settings::default());
        let spec = np.spec.unwrap();
        assert_eq!(
            spec.policy_types.as_ref().unwrap(),
            &vec!["Ingress".to_string(), "Egress".to_string()]
        );
        // Ingress allows the caliband port.
        let ingress = spec.ingress.unwrap();
        let iports = ingress[0].ports.clone().unwrap();
        assert!(iports
            .iter()
            .any(|p| p.port == Some(IntOrString::Int(8443))));
        // Ingress is scoped to the task's own namespace (same-namespace peer).
        let peers = ingress[0].from.clone().unwrap();
        assert_eq!(peers.len(), 1);
        assert!(peers[0].pod_selector.is_some());
        assert!(peers[0].namespace_selector.is_none());
        // Egress: DNS rule + an allow-all rule (empty `to`).
        let egress = spec.egress.unwrap();
        assert_eq!(egress.len(), 2);
        assert!(egress[1].to.is_none()); // allow-all destinations
                                         // Selects the pod by our managed labels.
        assert!(spec
            .pod_selector
            .unwrap()
            .match_labels
            .unwrap()
            .contains_key("caliban.caliban-ai.dev/task"));
    }

    #[test]
    fn sandbox_has_caliband_container_pvc_and_service() {
        let s = Settings::default();
        let sb = build_sandbox(&task(), &s);
        assert_eq!(sb.metadata.name.as_deref(), Some("refactor-auth-sbx"));
        assert_eq!(sb.metadata.namespace.as_deref(), Some("team-a"));
        assert_eq!(sb.spec.service, Some(true));
        let pod = sb.spec.pod_template.spec.unwrap();
        assert_eq!(
            pod.service_account_name.as_deref(),
            Some("refactor-auth-sa")
        );
        assert_eq!(pod.automount_service_account_token, Some(false));
        let c = &pod.containers[0];
        assert_eq!(
            c.image.as_deref(),
            Some("ghcr.io/caliban-ai/caliban:latest")
        );
        assert_eq!(c.ports.as_ref().unwrap()[0].container_port, 8443);
        // Args run caliband as a daemon: --workspace-root <root> --listen 0.0.0.0:<port>.
        let args = c.args.as_ref().unwrap();
        let root_idx = args
            .iter()
            .position(|a| a == "--workspace-root")
            .expect("--workspace-root flag present");
        assert_eq!(args[root_idx + 1], s.workspace_root);
        let listen_idx = args
            .iter()
            .position(|a| a == "--listen")
            .expect("--listen flag present");
        assert_eq!(args[listen_idx + 1], "0.0.0.0:8443");
        // The mismatched env vars from #283 must not reappear.
        let env = c.env.as_ref().unwrap();
        assert!(!env.iter().any(|e| e.name == "CALIBAND_LISTEN"));
        assert!(!env.iter().any(|e| e.name == "CALIBAN_WORKSPACE_ROOT"));
        assert!(!env.iter().any(|e| e.name == "CALIBAN_WORKSPACE_SOURCES"));
        // No model configured in the default fixture → no router-config env.
        assert!(!env.iter().any(|e| e.name == "CALIBAN_ROUTER_CONFIG_REF"));
        // Workspace PVC present.
        let pvcs = sb.spec.volume_claim_templates.unwrap();
        assert_eq!(pvcs[0].metadata.name.as_deref(), Some("workspace"));
        // Pod carries the managed labels (so the NetworkPolicy selects it).
        assert!(sb
            .spec
            .pod_template
            .metadata
            .unwrap()
            .labels
            .unwrap()
            .contains_key("caliban.caliban-ai.dev/task"));
    }

    #[test]
    fn sandbox_has_git_clone_init_container_for_workspace_sources() {
        let s = Settings::default();
        let sb = build_sandbox(&task(), &s);
        let pod = sb.spec.pod_template.spec.unwrap();
        let inits = pod.init_containers.expect("init containers present");
        assert_eq!(inits.len(), 1);
        let init = &inits[0];
        assert_eq!(init.name, "clone-workspace");
        assert_eq!(
            init.image.as_deref(),
            Some(Settings::default().git_image.as_str())
        );
        let mounts = init.volume_mounts.as_ref().unwrap();
        assert_eq!(mounts.len(), 1);
        assert_eq!(mounts[0].name, "workspace");
        assert_eq!(mounts[0].mount_path, s.workspace_root);
        let command = init.command.as_ref().unwrap();
        assert_eq!(command[0], "/bin/sh");
        assert_eq!(command[1], "-c");
        let script = &command[2];
        assert!(
            script.contains("git clone --depth 1 --branch 'main' 'git@x:caliban' '/work/caliban'")
        );
        assert!(script.contains("[ ! -d '/work/caliban/.git' ]"));
    }

    #[test]
    fn clone_script_shell_escapes_source_values() {
        let mut t = task();
        t.spec.workspace.sources[0].repo = "https://x/a'b".into();
        let sb = build_sandbox(&t, &Settings::default());
        let pod = sb.spec.pod_template.spec.unwrap();
        let init = &pod.init_containers.unwrap()[0];
        let script = &init.command.as_ref().unwrap()[2];
        assert!(script.contains("'https://x/a'\\''b'"));
        assert!(!script.contains("'https://x/a'b'"));
    }

    #[test]
    fn sandbox_omits_init_containers_when_no_workspace_sources() {
        let mut t = task();
        t.spec.workspace.sources = vec![];
        let sb = build_sandbox(&t, &Settings::default());
        let pod = sb.spec.pod_template.spec.unwrap();
        assert!(pod.init_containers.is_none());
    }

    #[test]
    fn sandbox_runtime_class_from_isolation() {
        use crate::crd::IsolationSpec;
        let mut t = task();
        t.spec.isolation = Some(IsolationSpec {
            runtime_class: Some("gvisor".into()),
            worktrees: None,
        });
        let sb = build_sandbox(&t, &Settings::default());
        assert_eq!(
            sb.spec
                .pod_template
                .spec
                .unwrap()
                .runtime_class_name
                .as_deref(),
            Some("gvisor")
        );
    }

    #[test]
    fn sandbox_projects_router_config_ref_env_when_model_set() {
        use crate::crd::ModelSpec;
        let mut t = task();
        t.spec.model = Some(ModelSpec {
            router_config_ref: Some("caliban-router".into()),
        });
        let sb = build_sandbox(&t, &Settings::default());
        let pod = sb.spec.pod_template.spec.unwrap();
        let env = pod.containers[0].env.as_ref().unwrap();
        assert!(env.iter().any(|e| e.name == "CALIBAN_ROUTER_CONFIG_REF"
            && e.value.as_deref() == Some("caliban-router")));
    }

    #[test]
    fn plan_names_all_three_children() {
        let p = plan(&task(), &Settings::default());
        assert_eq!(
            p.service_account.metadata.name.as_deref(),
            Some("refactor-auth-sa")
        );
        assert_eq!(
            p.network_policy.metadata.name.as_deref(),
            Some("refactor-auth-netpol")
        );
        assert_eq!(
            p.sandbox.metadata.name.as_deref(),
            Some("refactor-auth-sbx")
        );
    }

    #[test]
    fn provider_env_projects_kind_url_model_and_secret_ref() {
        use crate::workspace::{CredentialsRef, ResolvedProvider};
        let rp = ResolvedProvider {
            name: "planner".into(),
            kind: "anthropic".into(),
            base_url: Some("https://api.anthropic.com".into()),
            model: Some("claude-opus-4-8".into()),
            credentials_ref: Some(CredentialsRef {
                secret_name: "anthropic-key".into(),
                key: "api-key".into(),
            }),
        };
        let env = provider_env(&rp);
        let get = |n: &str| env.iter().find(|e| e.name == n).cloned();
        assert_eq!(
            get("CALIBAN_PROVIDER").unwrap().value.as_deref(),
            Some("anthropic")
        );
        assert_eq!(
            get("CALIBAN_PROVIDER_BASE_URL").unwrap().value.as_deref(),
            Some("https://api.anthropic.com")
        );
        assert_eq!(
            get("CALIBAN_MODEL").unwrap().value.as_deref(),
            Some("claude-opus-4-8")
        );
        // Secret reaches the pod by reference, never inlined.
        let key = get("CALIBAN_API_KEY").unwrap();
        assert!(key.value.is_none());
        let sel = key.value_from.unwrap().secret_key_ref.unwrap();
        assert_eq!(sel.name, "anthropic-key");
        assert_eq!(sel.key, "api-key");
    }

    #[test]
    fn provider_env_keyless_has_no_api_key() {
        use crate::workspace::ResolvedProvider;
        let rp = ResolvedProvider {
            name: "workers".into(),
            kind: "ollama".into(),
            base_url: Some("http://192.168.1.240:11434".into()),
            model: None,
            credentials_ref: None,
        };
        let env = provider_env(&rp);
        assert!(!env.iter().any(|e| e.name == "CALIBAN_API_KEY"));
        assert!(!env.iter().any(|e| e.name == "CALIBAN_MODEL"));
        assert_eq!(
            env.iter()
                .find(|e| e.name == "CALIBAN_PROVIDER")
                .unwrap()
                .value
                .as_deref(),
            Some("ollama")
        );
    }
}
