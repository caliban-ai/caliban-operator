//! Pure builders mapping a `CalibanTask` to the child objects a reconcile
//! applies: a token-less ServiceAccount, a default-deny NetworkPolicy, and the
//! backing agent-sandbox Sandbox. No cluster access — unit-tested. See ADR 0002.

use std::collections::BTreeMap;

use k8s_openapi::api::core::v1::{
    ConfigMapVolumeSource, Container, ContainerPort, EnvVar, EnvVarSource,
    PersistentVolumeClaimSpec, PodSpec, PodTemplateSpec, Probe, ResourceRequirements,
    SecretKeySelector, SecretVolumeSource, ServiceAccount, TCPSocketAction, Volume, VolumeMount,
    VolumeResourceRequirements,
};
use k8s_openapi::api::networking::v1::{
    NetworkPolicy, NetworkPolicyEgressRule, NetworkPolicyIngressRule, NetworkPolicyPeer,
    NetworkPolicyPort, NetworkPolicySpec,
};
use k8s_openapi::apimachinery::pkg::api::resource::Quantity;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{LabelSelector, ObjectMeta};
use k8s_openapi::apimachinery::pkg::util::intstr::IntOrString;
use kube::ResourceExt;

use crate::config::{
    caliband_advertise_host, common_labels, netpol_name, owner_ref, sa_name, sandbox_name, Settings,
};
use crate::crd::CalibanTask;
use crate::sandbox::{Sandbox, SandboxSpec, VolumeClaimTemplate};
use crate::workspace::{ResolvedProvider, ResolvedWorkspace};

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

/// A `[start, end]` inclusive port range (for caliband's per-agent stream window).
fn np_port_range(proto: &str, start: i32, end: i32) -> NetworkPolicyPort {
    NetworkPolicyPort {
        protocol: Some(proto.to_string()),
        port: Some(IntOrString::Int(start)),
        end_port: Some(end),
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
            // Ingress: the caliband control port plus the per-agent stream window
            // prosperod dials once it resolves an agent's endpoint (#25). Without
            // the range, the stream dial is blocked even with a routable host.
            ingress: Some(vec![NetworkPolicyIngressRule {
                ports: Some(vec![
                    np_port("TCP", s.caliband_port),
                    np_port_range("TCP", s.agent_port_base, s.agent_port_end),
                ]),
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

/// Volume + mount for the ConfigMap named by `spec.model.routerConfigRef` (#44).
const ROUTER_CONFIG_VOLUME: &str = "router-config";
const ROUTER_CONFIG_MOUNT: &str = "/etc/caliban/router";
/// The ConfigMap key caliban loads — the same `caliban.toml` its own discovery
/// walks for. caliban reads `CALIBAN_ROUTER_CONFIG` as a *path* to this file.
const ROUTER_CONFIG_KEY: &str = "caliban.toml";

fn router_config_ref(t: &CalibanTask) -> Option<&str> {
    t.spec
        .model
        .as_ref()
        .and_then(|m| m.router_config_ref.as_deref())
}

fn env(name: &str, value: String) -> EnvVar {
    EnvVar {
        name: name.to_string(),
        value: Some(value),
        ..Default::default()
    }
}

fn caliband_env(t: &CalibanTask, rw: &ResolvedWorkspace) -> Vec<EnvVar> {
    let mut e = vec![];
    if let Some(ep) = t
        .spec
        .state
        .as_ref()
        .and_then(|st| st.gonzalo_endpoint.clone())
    {
        e.push(env("GONZALO_ENDPOINT", ep));
    }
    if router_config_ref(t).is_some() {
        e.push(env(
            "CALIBAN_ROUTER_CONFIG",
            format!("{ROUTER_CONFIG_MOUNT}/{ROUTER_CONFIG_KEY}"),
        ));
    }
    e.extend(provider_env(&rw.provider));
    for kv in &rw.env {
        e.push(env(&kv.name, kv.value.clone()));
    }
    e
}

/// The env-name prefix caliban reads for a provider kind — the `{PROVIDER}` in
/// `{PROVIDER}_BASE_URL` / `{PROVIDER}_API_KEY` (#30, caliban#390's acceptance).
///
/// Uppercasing the kind is right for `anthropic` and `openai`, but
/// two kinds are irregular on caliban's side and must be aliased explicitly:
/// its google provider reads the `GEMINI_*` pair, and its Azure path reads
/// `AZURE_OPENAI_*`. An unknown kind falls back to the uppercase form, which is
/// the convention every provider crate follows.
fn provider_env_prefix(kind: &str) -> String {
    match kind.to_ascii_lowercase().as_str() {
        "google" | "gemini" => "GEMINI".to_string(),
        "azure" | "azure-openai" | "azure_openai" => "AZURE_OPENAI".to_string(),
        other => other.to_ascii_uppercase().replace(['-', '.'], "_"),
    }
}

/// Project a resolved provider to caliband container env. Credentials reach the
/// pod via `secretKeyRef` (the operator never inlines the value).
///
/// Base URL and API key are projected under **provider-native** names (#30).
/// caliban implements no `CALIBAN_*` namespace for these: it reads
/// `OPENAI_BASE_URL`, `ANTHROPIC_BASE_URL`/`ANTHROPIC_API_KEY`, and so on.
/// Projecting `CALIBAN_PROVIDER_BASE_URL` / `CALIBAN_API_KEY` silently dropped
/// both the endpoint and the credential — the pod looked correctly configured
/// while the agent could reach nothing.
///
/// `CALIBAN_MODEL` is not projected at all: caliban never reads it, and the
/// model reaches the worker through caliband's `SpawnSpec` (prospero#168).
/// `CALIBAN_PROVIDER` *is* still projected — not as a provider selector
/// (caliban#93 verified it is not one; selection travels in the `SpawnSpec`)
/// but because it is the documented input to a user-configured `apiKeyHelper`.
pub(crate) fn provider_env(rp: &ResolvedProvider) -> Vec<EnvVar> {
    let prefix = provider_env_prefix(&rp.kind);
    let mut e = vec![env("CALIBAN_PROVIDER", rp.kind.clone())];
    if let Some(u) = &rp.base_url {
        e.push(env(&format!("{prefix}_BASE_URL"), u.clone()));
    }
    if let Some(c) = &rp.credentials_ref {
        e.push(EnvVar {
            name: format!("{prefix}_API_KEY"),
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
fn clone_script(rw: &ResolvedWorkspace) -> String {
    let mut s = String::from("set -eu\n");
    for src in &rw.sources {
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

/// The git-clone init container that populates the workspace volume from the
/// resolved workspace's `sources[]`, if any are configured.
fn clone_init_container(rw: &ResolvedWorkspace, s: &Settings) -> Option<Container> {
    if rw.sources.is_empty() {
        return None;
    }
    Some(Container {
        name: "clone-workspace".to_string(),
        image: Some(s.git_image.clone()),
        command: Some(vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            clone_script(rw),
        ]),
        volume_mounts: Some(vec![VolumeMount {
            name: WORKSPACE_VOLUME.to_string(),
            mount_path: s.workspace_root.clone(),
            ..Default::default()
        }]),
        resources: sandbox_resources(s),
        ..Default::default()
    })
}

/// CPU/memory requests and limits for both sandbox containers (#50). Without
/// them every agent pod is BestEffort QoS: first evicted under node pressure and
/// refused by namespaces that enforce a ResourceQuota. `None` when nothing is
/// configured, so the pod carries no empty `resources` block.
///
/// The clone init container gets the same values: a pod's effective request is
/// the max of any init container and the sum of app containers, so this adds
/// nothing to scheduling while still admitting the init step under quota.
fn sandbox_resources(s: &Settings) -> Option<ResourceRequirements> {
    fn quantities(cpu: Option<&str>, memory: Option<&str>) -> Option<BTreeMap<String, Quantity>> {
        let m: BTreeMap<String, Quantity> = [("cpu", cpu), ("memory", memory)]
            .into_iter()
            .filter_map(|(k, v)| v.map(|q| (k.to_string(), Quantity(q.to_string()))))
            .collect();
        (!m.is_empty()).then_some(m)
    }
    let requests = quantities(
        s.caliband_cpu_request.as_deref(),
        s.caliband_memory_request.as_deref(),
    );
    let limits = quantities(
        s.caliband_cpu_limit.as_deref(),
        s.caliband_memory_limit.as_deref(),
    );
    (requests.is_some() || limits.is_some()).then(|| ResourceRequirements {
        requests,
        limits,
        ..Default::default()
    })
}

/// A TCP probe on caliband's control port (#45).
fn control_port_probe(s: &Settings, period_seconds: i32, failure_threshold: i32) -> Probe {
    Probe {
        tcp_socket: Some(TCPSocketAction {
            port: IntOrString::Int(s.caliband_port),
            ..Default::default()
        }),
        period_seconds: Some(period_seconds),
        failure_threshold: Some(failure_threshold),
        ..Default::default()
    }
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

/// Map a `CalibanTask` + its resolved workspace to the backing agent-sandbox
/// `Sandbox`.
pub fn build_sandbox(t: &CalibanTask, rw: &ResolvedWorkspace, s: &Settings) -> Sandbox {
    const TLS_MOUNT: &str = "/etc/caliband/tls";
    const TLS_VOLUME: &str = "session-tls";

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
            // Advertise the pod's routable DNS (not the 0.0.0.0 bind) so prosperod
            // can reach the per-agent stream endpoints caliband hands out (#24).
            "--advertise-host".to_string(),
            caliband_advertise_host(t, s),
            // Pin the per-agent port base so it stays locked to the window the
            // NetworkPolicy opens (#25) — the operator is the single source of truth.
            "--agent-port-base".to_string(),
            s.agent_port_base.to_string(),
            "--tls-cert".to_string(),
            format!("{TLS_MOUNT}/tls.crt"),
            "--tls-key".to_string(),
            format!("{TLS_MOUNT}/tls.key"),
            // Hand caliband its own trust anchor (#32). Without a CA it cannot
            // pass one down to the workers it spawns, so their status reports
            // fail the handshake and an interactive agent never reaches Idle.
            // The key is already in the mounted session-plane Secret.
            "--tls-ca".to_string(),
            format!("{TLS_MOUNT}/ca.crt"),
            // ...and the name that CA must vouch for (#35). caliband defaults
            // this to `--advertise-host` — the per-pod DNS name — but the
            // serving cert's only SAN is the session-plane name, so the default
            // is unverifiable here. caliban#510's launcher passes caliband's
            // resolved name to each worker *explicitly*, overriding whatever the
            // worker would have inherited from `CALIBAN_CONTROL_TLS_SERVER_NAME`
            // in the pod env below. That makes argv the value that actually
            // wins: setting only the env var leaves workers verifying against a
            // name the cert cannot prove, the handshake fails, and — the status
            // sink being best-effort — every Idle report is silently dropped.
            "--tls-server-name".to_string(),
            s.session_server_name.clone(),
        ]),
        env: Some({
            let mut e = caliband_env(t, rw);
            e.push(EnvVar {
                name: "CALIBAN_DAEMON_TOKEN".to_string(),
                value: None,
                value_from: Some(EnvVarSource {
                    secret_key_ref: Some(SecretKeySelector {
                        name: s.session_token_secret.clone(),
                        key: s.session_token_key.clone(),
                        optional: Some(false),
                    }),
                    ..Default::default()
                }),
            });
            // No `CALIBAN_CONTROL_TLS_*` here (#59): caliband ≥ 0.8.0 sets both
            // on each worker it spawns, from `--tls-ca` / `--tls-server-name`
            // above (caliban#510, #512). A pod-env copy would be a second,
            // overridden mechanism that could only drift from the argv.
            e
        }),
        volume_mounts: Some({
            let mut m = vec![
                VolumeMount {
                    name: WORKSPACE_VOLUME.to_string(),
                    mount_path: s.workspace_root.clone(),
                    ..Default::default()
                },
                VolumeMount {
                    name: TLS_VOLUME.to_string(),
                    mount_path: TLS_MOUNT.to_string(),
                    read_only: Some(true),
                    ..Default::default()
                },
            ];
            if router_config_ref(t).is_some() {
                m.push(VolumeMount {
                    name: ROUTER_CONFIG_VOLUME.to_string(),
                    mount_path: ROUTER_CONFIG_MOUNT.to_string(),
                    read_only: Some(true),
                    ..Default::default()
                });
            }
            m
        }),
        resources: sandbox_resources(s),
        // #45: probe the control port so the pod — and agent-sandbox's `Ready`
        // condition, which the task's `Running` phase gates on — only reports
        // ready once caliband has actually bound it. Startup tolerates a slow
        // first bind (up to ~2 min) without the readiness probe flapping.
        // No liveness probe: restarting caliband kills every agent it runs.
        readiness_probe: Some(control_port_probe(s, 5, 3)),
        startup_probe: Some(control_port_probe(s, 2, 60)),
        ..Default::default()
    };
    let mut volumes = vec![Volume {
        name: TLS_VOLUME.to_string(),
        secret: Some(SecretVolumeSource {
            secret_name: Some(s.session_tls_secret.clone()),
            ..Default::default()
        }),
        ..Default::default()
    }];
    if let Some(cm) = router_config_ref(t) {
        volumes.push(Volume {
            name: ROUTER_CONFIG_VOLUME.to_string(),
            config_map: Some(ConfigMapVolumeSource {
                name: cm.to_string(),
                ..Default::default()
            }),
            ..Default::default()
        });
    }
    let pod_spec = PodSpec {
        containers: vec![container],
        init_containers: clone_init_container(rw, s).map(|c| vec![c]),
        // The task's per-run override takes precedence over the workspace default.
        runtime_class_name: t
            .spec
            .isolation
            .as_ref()
            .and_then(|i| i.runtime_class.clone())
            .or_else(|| rw.isolation.as_ref().and_then(|i| i.runtime_class.clone())),
        service_account_name: Some(sa_name(t)),
        automount_service_account_token: Some(false),
        volumes: Some(volumes),
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
pub fn plan(t: &CalibanTask, rw: &ResolvedWorkspace, s: &Settings) -> ReconcilePlan {
    ReconcilePlan {
        service_account: build_service_account(t),
        network_policy: build_network_policy(t, s),
        sandbox: build_sandbox(t, rw, s),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crd::{CalibanTaskSpec, TaskSpec, WorkspaceRef};
    use crate::workspace::Source;

    fn task() -> CalibanTask {
        let mut t = CalibanTask::new(
            "refactor-auth",
            CalibanTaskSpec {
                workspace_ref: WorkspaceRef {
                    name: "team-a-ws".into(),
                },
                provider_ref: None,
                task: TaskSpec {
                    prompt: "hi".into(),
                    agent_type: None,
                    interactive: None,
                },
                model: None,
                state: None,
                isolation: None,
                resources: None,
                lifecycle: None,
                tools: None,
            },
        );
        t.metadata.namespace = Some("team-a".into());
        t.metadata.uid = Some("uid-123".into());
        t
    }

    fn resolved() -> crate::workspace::ResolvedWorkspace {
        use crate::workspace::{ResolvedProvider, ResolvedWorkspace};
        ResolvedWorkspace {
            sources: vec![Source {
                name: "caliban".into(),
                repo: "git@x:caliban".into(),
                r#ref: "main".into(),
                path: "/work/caliban".into(),
            }],
            provider: ResolvedProvider {
                name: "workers".into(),
                kind: "openai".into(),
                base_url: None,
                model: None,
                credentials_ref: None,
            },
            env: vec![],
            isolation: None,
        }
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
    fn network_policy_opens_the_per_agent_stream_port_range() {
        // #25: prosperod's per-agent stream dial targets a port drawn from the
        // agent-port-base (7100) upward, which the old policy (8443-only) blocked.
        let np = build_network_policy(&task(), &Settings::default());
        let ingress = np.spec.unwrap().ingress.unwrap();
        let ports = ingress[0].ports.clone().unwrap();
        // The caliband control port is still allowed as a single port.
        assert!(ports
            .iter()
            .any(|p| p.port == Some(IntOrString::Int(8443)) && p.end_port.is_none()));
        // The per-agent stream window 7100..=7999 is allowed as a range.
        assert!(ports
            .iter()
            .any(|p| p.port == Some(IntOrString::Int(7100)) && p.end_port == Some(7999)));
    }

    #[test]
    fn sandbox_advertises_routable_host_and_pins_agent_port_base() {
        let s = Settings::default();
        let sb = build_sandbox(&task(), &resolved(), &s);
        let pod = sb.spec.pod_template.spec.unwrap();
        let args = pod.containers[0].args.as_ref().unwrap();
        // #24: caliband advertises the pod's routable DNS for per-agent endpoints,
        // not its 0.0.0.0 bind address.
        let adv_idx = args
            .iter()
            .position(|a| a == "--advertise-host")
            .expect("--advertise-host flag present");
        assert_eq!(
            args[adv_idx + 1],
            "refactor-auth-sbx.team-a.svc.cluster.local"
        );
        // The operator pins the port base so it stays locked to the NetworkPolicy window.
        let base_idx = args
            .iter()
            .position(|a| a == "--agent-port-base")
            .expect("--agent-port-base flag present");
        assert_eq!(args[base_idx + 1], "7100");
    }

    /// #45: with no probe, Kubernetes has no independent signal that caliband has
    /// bound its control port, so the pod — and in turn the Sandbox's `Ready`
    /// condition — reports ready while a crash-looping caliband cannot answer.
    #[test]
    fn caliband_container_has_tcp_readiness_and_startup_probes_on_the_control_port() {
        let s = Settings::default();
        let sb = build_sandbox(&task(), &resolved(), &s);
        let pod = sb.spec.pod_template.spec.unwrap();
        let c = &pod.containers[0];
        for (kind, probe) in [
            ("readiness", c.readiness_probe.as_ref()),
            ("startup", c.startup_probe.as_ref()),
        ] {
            let probe = probe.unwrap_or_else(|| panic!("caliband has no {kind} probe"));
            let tcp = probe
                .tcp_socket
                .as_ref()
                .unwrap_or_else(|| panic!("{kind} probe is not a tcpSocket probe"));
            assert_eq!(tcp.port, IntOrString::Int(s.caliband_port), "{kind}");
        }
        // Liveness is deliberately absent: restarting caliband would kill every
        // agent it supervises, which a readiness gate alone does not.
        assert!(c.liveness_probe.is_none());
    }

    #[test]
    fn sandbox_has_caliband_container_pvc_and_service() {
        let s = Settings::default();
        let sb = build_sandbox(&task(), &resolved(), &s);
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
        assert!(!env.iter().any(|e| e.name == "CALIBAN_ROUTER_CONFIG"));
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
        let sb = build_sandbox(&task(), &resolved(), &s);
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
        let t = task();
        let mut rw = resolved();
        rw.sources[0].repo = "https://x/a'b".into();
        let sb = build_sandbox(&t, &rw, &Settings::default());
        let pod = sb.spec.pod_template.spec.unwrap();
        let init = &pod.init_containers.unwrap()[0];
        let script = &init.command.as_ref().unwrap()[2];
        assert!(script.contains("'https://x/a'\\''b'"));
        assert!(!script.contains("'https://x/a'b'"));
    }

    #[test]
    fn sandbox_omits_init_containers_when_no_workspace_sources() {
        let t = task();
        let mut rw = resolved();
        rw.sources = vec![];
        let sb = build_sandbox(&t, &rw, &Settings::default());
        let pod = sb.spec.pod_template.spec.unwrap();
        assert!(pod.init_containers.is_none());
    }

    /// Both sandbox containers: caliband, then the git-clone init container.
    fn sandbox_containers(s: &Settings) -> Vec<Container> {
        let sb = build_sandbox(&task(), &resolved(), s);
        let pod = sb.spec.pod_template.spec.unwrap();
        let mut cs = pod.containers;
        cs.extend(pod.init_containers.expect("init container present"));
        assert_eq!(cs.len(), 2);
        cs
    }

    /// #50: with no `resources` block every sandbox pod is BestEffort QoS —
    /// first evicted under node pressure and rejected outright by namespaces
    /// that enforce a ResourceQuota.
    #[test]
    fn default_settings_give_both_containers_requests_but_no_limits() {
        for c in sandbox_containers(&Settings::default()) {
            let r = c
                .resources
                .as_ref()
                .unwrap_or_else(|| panic!("{} has no resources block", c.name));
            let req = r.requests.as_ref().expect("requests");
            assert_eq!(req.get("cpu"), Some(&Quantity("250m".into())), "{}", c.name);
            assert_eq!(
                req.get("memory"),
                Some(&Quantity("512Mi".into())),
                "{}",
                c.name
            );
            assert!(
                r.limits.is_none(),
                "{}: defaults must not set limits (Burstable, not a surprise OOM)",
                c.name
            );
        }
    }

    #[test]
    fn configured_limits_are_applied_to_both_containers() {
        let s = Settings {
            caliband_cpu_limit: Some("2".into()),
            caliband_memory_limit: Some("4Gi".into()),
            ..Settings::default()
        };
        for c in sandbox_containers(&s) {
            let limits = c
                .resources
                .as_ref()
                .and_then(|r| r.limits.as_ref())
                .unwrap_or_else(|| panic!("{} has no limits", c.name));
            assert_eq!(limits.get("cpu"), Some(&Quantity("2".into())), "{}", c.name);
            assert_eq!(
                limits.get("memory"),
                Some(&Quantity("4Gi".into())),
                "{}",
                c.name
            );
        }
    }

    #[test]
    fn with_no_requests_or_limits_the_resources_block_is_omitted() {
        let s = Settings {
            caliband_cpu_request: None,
            caliband_memory_request: None,
            ..Settings::default()
        };
        for c in sandbox_containers(&s) {
            assert!(
                c.resources.is_none(),
                "{}: expected no resources block",
                c.name
            );
        }
    }

    #[test]
    fn sandbox_runtime_class_from_isolation() {
        use crate::crd::IsolationSpec;
        let mut t = task();
        t.spec.isolation = Some(IsolationSpec {
            runtime_class: Some("gvisor".into()),
            worktrees: None,
        });
        let sb = build_sandbox(&t, &resolved(), &Settings::default());
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
    fn sandbox_runtime_class_falls_back_to_workspace_isolation() {
        use crate::crd::IsolationSpec;
        let t = task();
        let mut rw = resolved();
        rw.isolation = Some(IsolationSpec {
            runtime_class: Some("kata".into()),
            worktrees: None,
        });
        let sb = build_sandbox(&t, &rw, &Settings::default());
        assert_eq!(
            sb.spec
                .pod_template
                .spec
                .unwrap()
                .runtime_class_name
                .as_deref(),
            Some("kata")
        );
    }

    #[test]
    fn sandbox_runtime_class_task_override_wins_over_workspace() {
        use crate::crd::IsolationSpec;
        let mut t = task();
        t.spec.isolation = Some(IsolationSpec {
            runtime_class: Some("gvisor".into()),
            worktrees: None,
        });
        let mut rw = resolved();
        rw.isolation = Some(IsolationSpec {
            runtime_class: Some("kata".into()),
            worktrees: None,
        });
        let sb = build_sandbox(&t, &rw, &Settings::default());
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

    /// #44: caliban reads `CALIBAN_ROUTER_CONFIG` as a *file path* to a
    /// `caliban.toml`. Projecting the ConfigMap's name under
    /// `CALIBAN_ROUTER_CONFIG_REF` reached nothing, so the ConfigMap must be
    /// mounted and the env must point at the file inside that mount.
    #[test]
    fn sandbox_mounts_router_config_map_and_points_caliban_at_the_file() {
        use crate::crd::ModelSpec;
        let mut t = task();
        t.spec.model = Some(ModelSpec {
            router_config_ref: Some("caliban-router".into()),
        });
        let sb = build_sandbox(&t, &resolved(), &Settings::default());
        let pod = sb.spec.pod_template.spec.unwrap();

        let vol = pod
            .volumes
            .as_ref()
            .unwrap()
            .iter()
            .find(|v| v.name == "router-config")
            .expect("router-config volume");
        let cm = vol.config_map.as_ref().expect("ConfigMap volume source");
        assert_eq!(cm.name, "caliban-router");

        let c = &pod.containers[0];
        let mount = c
            .volume_mounts
            .as_ref()
            .unwrap()
            .iter()
            .find(|m| m.name == "router-config")
            .expect("router-config mount");
        assert_eq!(mount.mount_path, "/etc/caliban/router");
        assert_eq!(mount.read_only, Some(true));

        let env = c.env.as_ref().unwrap();
        let path = env
            .iter()
            .find(|e| e.name == "CALIBAN_ROUTER_CONFIG")
            .and_then(|e| e.value.as_deref())
            .expect("CALIBAN_ROUTER_CONFIG");
        assert_eq!(path, "/etc/caliban/router/caliban.toml");
        assert!(path.starts_with(&format!("{}/", mount.mount_path)));
        assert!(
            !env.iter().any(|e| e.name == "CALIBAN_ROUTER_CONFIG_REF"),
            "the inert _REF name must not be emitted"
        );
    }

    #[test]
    fn sandbox_without_router_config_ref_adds_no_router_volume_or_env() {
        let sb = build_sandbox(&task(), &resolved(), &Settings::default());
        let pod = sb.spec.pod_template.spec.unwrap();
        assert!(!pod
            .volumes
            .unwrap_or_default()
            .iter()
            .any(|v| v.name == "router-config"));
        let c = &pod.containers[0];
        assert!(!c
            .volume_mounts
            .as_ref()
            .unwrap()
            .iter()
            .any(|m| m.name == "router-config"));
        assert!(!c
            .env
            .as_ref()
            .unwrap()
            .iter()
            .any(|e| e.name.starts_with("CALIBAN_ROUTER_CONFIG")));
    }

    #[test]
    fn sandbox_projects_resolved_provider_and_workspace_env() {
        use crate::workspace::EnvEntry;
        let t = task();
        let mut rw = resolved();
        rw.env = vec![EnvEntry {
            name: "FOO".into(),
            value: "bar".into(),
        }];
        let sb = build_sandbox(&t, &rw, &Settings::default());
        let pod = sb.spec.pod_template.spec.unwrap();
        let env = pod.containers[0].env.as_ref().unwrap();
        assert!(env
            .iter()
            .any(|e| e.name == "CALIBAN_PROVIDER" && e.value.as_deref() == Some("openai")));
        assert!(env
            .iter()
            .any(|e| e.name == "FOO" && e.value.as_deref() == Some("bar")));
    }

    #[test]
    fn sandbox_wires_session_plane_tls_and_token() {
        let s = Settings::default();
        let sb = build_sandbox(&task(), &resolved(), &s);
        let pod = sb.spec.pod_template.spec.unwrap();
        let c = &pod.containers[0];

        // TLS args present and pointing at the mounted files.
        let args = c.args.as_ref().unwrap();
        let cert_idx = args
            .iter()
            .position(|a| a == "--tls-cert")
            .expect("--tls-cert");
        assert_eq!(args[cert_idx + 1], "/etc/caliband/tls/tls.crt");
        let key_idx = args
            .iter()
            .position(|a| a == "--tls-key")
            .expect("--tls-key");
        assert_eq!(args[key_idx + 1], "/etc/caliband/tls/tls.key");

        // Bearer token injected by reference (never inlined).
        let env = c.env.as_ref().unwrap();
        let tok = env
            .iter()
            .find(|e| e.name == "CALIBAN_DAEMON_TOKEN")
            .expect("token env");
        let sel = tok
            .value_from
            .as_ref()
            .unwrap()
            .secret_key_ref
            .as_ref()
            .unwrap();
        assert_eq!(sel.name, "caliban-session-plane-token");
        assert_eq!(sel.key, "token");
        assert!(
            tok.value.is_none(),
            "token must never be inlined as plaintext"
        );

        // TLS Secret mounted read-only at the expected path.
        let mount = c
            .volume_mounts
            .as_ref()
            .unwrap()
            .iter()
            .find(|m| m.mount_path == "/etc/caliband/tls")
            .expect("tls mount");
        assert_eq!(mount.read_only, Some(true));
        let vol = pod
            .volumes
            .as_ref()
            .unwrap()
            .iter()
            .find(|v| v.name == mount.name)
            .expect("tls volume");
        assert_eq!(
            vol.secret.as_ref().unwrap().secret_name.as_deref(),
            Some("caliban-session-plane-tls")
        );
    }

    /// #32: caliband was launched with cert+key but no CA, so it had no trust
    /// anchor to hand down to the workers it spawns. The CA is already in the
    /// mounted `kubernetes.io/tls` Secret — only the flag was missing.
    #[test]
    fn sandbox_passes_session_plane_ca_to_caliband() {
        let sb = build_sandbox(&task(), &resolved(), &Settings::default());
        let pod = sb.spec.pod_template.spec.unwrap();
        let args = pod.containers[0].args.as_ref().unwrap();
        let ca_idx = args.iter().position(|a| a == "--tls-ca").expect("--tls-ca");
        assert_eq!(args[ca_idx + 1], "/etc/caliband/tls/ca.crt");
    }

    /// #59: caliband ≥ 0.8.0 (caliban#510, #512) sets `CALIBAN_CONTROL_TLS_CA`
    /// and `CALIBAN_CONTROL_TLS_SERVER_NAME` on each worker it spawns, explicitly,
    /// from its own `--tls-ca` / `--tls-server-name`. The operator passes both
    /// flags, so publishing the same values in the pod env is a second,
    /// overridden mechanism that can only drift from the argv. It must be gone.
    #[test]
    fn caliband_env_carries_no_control_plane_tls_vars() {
        let sb = build_sandbox(&task(), &resolved(), &Settings::default());
        let pod = sb.spec.pod_template.spec.unwrap();
        let env = pod.containers[0].env.as_ref().unwrap();
        for name in ["CALIBAN_CONTROL_TLS_CA", "CALIBAN_CONTROL_TLS_SERVER_NAME"] {
            assert!(
                !env.iter().any(|e| e.name == name),
                "{name} belongs to caliband's worker launcher, not the pod env"
            );
        }
    }

    /// #35: caliban#510's launcher sets each worker's
    /// `CALIBAN_CONTROL_TLS_SERVER_NAME` explicitly from caliband's own resolved
    /// name. That name must be the serving cert's SAN: the session-plane name,
    /// never the per-pod advertise host. A mismatch fails the handshake and,
    /// the status sink being best-effort, every Idle report is silently
    /// dropped. So it must be on caliband's command line.
    #[test]
    fn sandbox_passes_session_server_name_to_caliband_on_the_command_line() {
        let sb = build_sandbox(&task(), &resolved(), &Settings::default());
        let pod = sb.spec.pod_template.spec.unwrap();
        let args = pod.containers[0].args.as_ref().unwrap();
        let idx = args
            .iter()
            .position(|a| a == "--tls-server-name")
            .expect("--tls-server-name must be on caliband's argv (#35)");
        assert_eq!(
            args[idx + 1],
            "caliband",
            "the verified name must be the serving cert's SAN"
        );
    }

    /// #35: and it must track the configured SAN, not be hardcoded.
    #[test]
    fn caliband_argv_server_name_follows_settings() {
        let s = Settings {
            session_server_name: "sessions.example.svc".to_string(),
            ..Settings::default()
        };
        let sb = build_sandbox(&task(), &resolved(), &s);
        let pod = sb.spec.pod_template.spec.unwrap();
        let args = pod.containers[0].args.as_ref().unwrap();
        let idx = args
            .iter()
            .position(|a| a == "--tls-server-name")
            .expect("--tls-server-name");
        assert_eq!(args[idx + 1], "sessions.example.svc");
    }

    #[test]
    fn plan_names_all_three_children() {
        let p = plan(&task(), &resolved(), &Settings::default());
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

    /// #30: caliban reads **provider-native** env names — `ANTHROPIC_BASE_URL`
    /// / `ANTHROPIC_API_KEY` for anthropic, and so on. It reads no
    /// `CALIBAN_PROVIDER_BASE_URL` or `CALIBAN_API_KEY` at all, so projecting
    /// under those names silently dropped both the endpoint and the credential.
    /// caliban#390's acceptance already specified `{PROVIDER}_BASE_URL`.
    #[test]
    fn provider_env_projects_base_url_and_key_under_provider_native_names() {
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
            get("ANTHROPIC_BASE_URL").unwrap().value.as_deref(),
            Some("https://api.anthropic.com"),
            "the base URL must land on the name caliban actually reads"
        );
        // Secret reaches the pod by reference, never inlined.
        let key = get("ANTHROPIC_API_KEY").expect("keyed provider projects its native API key env");
        assert!(key.value.is_none());
        let sel = key.value_from.unwrap().secret_key_ref.unwrap();
        assert_eq!(sel.name, "anthropic-key");
        assert_eq!(sel.key, "api-key");

        // The invented CALIBAN_* namespace caliban never implemented.
        assert!(
            get("CALIBAN_PROVIDER_BASE_URL").is_none(),
            "caliban never reads CALIBAN_PROVIDER_BASE_URL"
        );
        assert!(
            get("CALIBAN_API_KEY").is_none(),
            "caliban never reads CALIBAN_API_KEY"
        );
        // The model travels in caliband's SpawnSpec (prospero#168), not in env.
        assert!(
            get("CALIBAN_MODEL").is_none(),
            "caliban never reads CALIBAN_MODEL"
        );
    }

    /// #30: the case that wedged the live cluster — a remote self-hosted
    /// (openai-compatible) provider whose base URL never reached the client.
    #[test]
    fn provider_env_projects_openai_base_url() {
        use crate::workspace::ResolvedProvider;
        let rp = ResolvedProvider {
            name: "workers".into(),
            kind: "openai".into(),
            base_url: Some("http://192.168.1.240:9292/v1".into()),
            model: None,
            credentials_ref: None,
        };
        let env = provider_env(&rp);
        assert_eq!(
            env.iter()
                .find(|e| e.name == "OPENAI_BASE_URL")
                .unwrap()
                .value
                .as_deref(),
            Some("http://192.168.1.240:9292/v1")
        );
    }

    /// #30: caliban's google provider reads the `GEMINI_*` pair, so a naive
    /// uppercase of the kind (`GOOGLE_*`) would miss it.
    #[test]
    fn provider_env_maps_google_kind_onto_the_gemini_env_pair() {
        use crate::workspace::{CredentialsRef, ResolvedProvider};
        let rp = ResolvedProvider {
            name: "g".into(),
            kind: "google".into(),
            base_url: Some("https://generativelanguage.googleapis.com".into()),
            model: None,
            credentials_ref: Some(CredentialsRef {
                secret_name: "g-key".into(),
                key: "api-key".into(),
            }),
        };
        let env = provider_env(&rp);
        assert!(env.iter().any(|e| e.name == "GEMINI_BASE_URL"));
        assert!(env.iter().any(|e| e.name == "GEMINI_API_KEY"));
        assert!(!env.iter().any(|e| e.name == "GOOGLE_BASE_URL"));
    }

    #[test]
    fn provider_env_keyless_has_no_api_key() {
        use crate::workspace::ResolvedProvider;
        let rp = ResolvedProvider {
            name: "workers".into(),
            kind: "openai".into(),
            base_url: Some("http://192.168.1.240:9292/v1".into()),
            model: None,
            credentials_ref: None,
        };
        let env = provider_env(&rp);
        assert!(!env.iter().any(|e| e.name.ends_with("_API_KEY")));
        // CALIBAN_PROVIDER is still projected: caliban does not use it to select
        // a provider (prospero#168 / caliban#93 — that travels in the SpawnSpec),
        // but it is the documented input to a user-configured `apiKeyHelper`.
        assert_eq!(
            env.iter()
                .find(|e| e.name == "CALIBAN_PROVIDER")
                .unwrap()
                .value
                .as_deref(),
            Some("openai")
        );
    }
}
