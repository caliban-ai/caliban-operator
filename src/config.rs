//! Operator-level settings (image, ports, workspace defaults) and helpers for
//! naming and owning the child objects a reconcile creates. See ADR 0002.

use std::collections::BTreeMap;

use k8s_openapi::apimachinery::pkg::apis::meta::v1::OwnerReference;
use kube::{Resource, ResourceExt};

use crate::crd::CalibanTask;

/// Runtime configuration, sourced from the environment with neutral defaults.
#[derive(Clone, Debug)]
pub struct Settings {
    /// Container image for the caliband pod.
    pub caliband_image: String,
    /// TCP+TLS port caliband listens on inside the pod.
    pub caliband_port: i32,
    /// Base port caliband draws per-agent stream listeners from (monotonic).
    pub agent_port_base: i32,
    /// Top of the per-agent stream port window the NetworkPolicy opens (inclusive).
    pub agent_port_end: i32,
    /// Workspace root mount path in the pod.
    pub workspace_root: String,
    /// Requested size of the workspace PVC (e.g. `10Gi`).
    pub workspace_storage: String,
    /// Container image for the git-clone init container that populates the workspace.
    pub git_image: String,
    /// Name of the shared TLS serving-cert Secret (keys tls.crt/tls.key/ca.crt).
    pub session_tls_secret: String,
    /// Name of the shared bearer-token Secret.
    pub session_token_secret: String,
    /// Key within the token Secret.
    pub session_token_key: String,
    /// Name the session-plane serving cert is verified against — must equal
    /// that cert's SAN (the chart's `global.sessionPlane.serverName`). Handed
    /// to caliband so workers can verify the control listener (#32).
    pub session_server_name: String,
    /// CPU request for the sandbox containers (e.g. `250m`). `None` omits it (#50).
    pub caliband_cpu_request: Option<String>,
    /// Memory request for the sandbox containers (e.g. `512Mi`). `None` omits it.
    pub caliband_memory_request: Option<String>,
    /// CPU limit for the sandbox containers. Unset by default.
    pub caliband_cpu_limit: Option<String>,
    /// Memory limit for the sandbox containers. Unset by default, so a default
    /// install never OOM-kills an agent on a guessed ceiling.
    pub caliband_memory_limit: Option<String>,
    /// The cluster's DNS domain, used to build caliband's advertise host
    /// (`{sandbox}.{namespace}.svc.{domain}`). Defaults to `cluster.local` (#54).
    pub cluster_domain: String,
    /// Labels a pod must carry to reach caliband (#57). Empty admits every pod
    /// in the peer namespace — the historical same-namespace behaviour.
    pub ingress_pod_selector: BTreeMap<String, String>,
    /// Namespace allowed to reach caliband (#57), matched by its
    /// `kubernetes.io/metadata.name` label. `None` means the task's own.
    pub ingress_namespace: Option<String>,
    /// Problems found while reading the environment, reported by
    /// [`Settings::validate`] at startup (`from_env` itself cannot fail).
    pub env_errors: Vec<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            caliband_image: "ghcr.io/caliban-ai/caliban:latest".to_string(),
            caliband_port: 8443,
            agent_port_base: 7100,
            agent_port_end: 7999,
            workspace_root: "/work".to_string(),
            workspace_storage: "10Gi".to_string(),
            git_image: "alpine/git:latest".to_string(),
            session_tls_secret: "caliban-session-plane-tls".to_string(),
            session_token_secret: "caliban-session-plane-token".to_string(),
            session_token_key: "token".to_string(),
            session_server_name: "caliband".to_string(),
            // Requests by default (Burstable QoS, schedulable under quota);
            // limits opt-in.
            caliband_cpu_request: Some("250m".to_string()),
            caliband_memory_request: Some("512Mi".to_string()),
            caliband_cpu_limit: None,
            caliband_memory_limit: None,
            cluster_domain: DEFAULT_CLUSTER_DOMAIN.to_string(),
            ingress_pod_selector: BTreeMap::new(),
            ingress_namespace: None,
            env_errors: Vec::new(),
        }
    }
}

impl Settings {
    /// Read settings from `CALIBAND_IMAGE`, `CALIBAND_PORT`, `CALIBAN_AGENT_PORT_BASE`,
    /// `CALIBAN_AGENT_PORT_END`, `CALIBAN_WORKSPACE_ROOT`, `CALIBAN_WORKSPACE_STORAGE`,
    /// `CALIBAN_GIT_IMAGE`, `CALIBAN_SESSION_TLS_SECRET`, `CALIBAN_SESSION_TOKEN_SECRET`,
    /// `CALIBAN_SESSION_TOKEN_KEY`, `CALIBAN_SESSION_SERVER_NAME`,
    /// `CALIBAND_CPU_REQUEST`, `CALIBAND_MEMORY_REQUEST`, `CALIBAND_CPU_LIMIT`,
    /// `CALIBAND_MEMORY_LIMIT`, `CALIBAN_CLUSTER_DOMAIN`,
    /// `CALIBAN_INGRESS_POD_SELECTOR`, `CALIBAN_INGRESS_NAMESPACE`, falling back
    /// to defaults. An empty resource value clears its default (see
    /// [`optional_quantity`]); a blank cluster domain keeps `cluster.local`; a
    /// malformed ingress selector is recorded in `env_errors`.
    pub fn from_env() -> Self {
        let d = Self::default();
        let mut env_errors = Vec::new();
        let ingress_pod_selector = match parse_selector(
            &std::env::var("CALIBAN_INGRESS_POD_SELECTOR").unwrap_or_default(),
        ) {
            Ok(labels) => labels,
            Err(e) => {
                env_errors.push(format!("CALIBAN_INGRESS_POD_SELECTOR: {e}"));
                BTreeMap::new()
            }
        };
        let ingress_namespace = std::env::var("CALIBAN_INGRESS_NAMESPACE")
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty());
        Self {
            caliband_image: std::env::var("CALIBAND_IMAGE").unwrap_or(d.caliband_image),
            caliband_port: std::env::var("CALIBAND_PORT")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(d.caliband_port),
            agent_port_base: std::env::var("CALIBAN_AGENT_PORT_BASE")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(d.agent_port_base),
            agent_port_end: std::env::var("CALIBAN_AGENT_PORT_END")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(d.agent_port_end),
            workspace_root: std::env::var("CALIBAN_WORKSPACE_ROOT").unwrap_or(d.workspace_root),
            workspace_storage: std::env::var("CALIBAN_WORKSPACE_STORAGE")
                .unwrap_or(d.workspace_storage),
            git_image: std::env::var("CALIBAN_GIT_IMAGE").unwrap_or(d.git_image),
            session_tls_secret: std::env::var("CALIBAN_SESSION_TLS_SECRET")
                .unwrap_or(d.session_tls_secret),
            session_token_secret: std::env::var("CALIBAN_SESSION_TOKEN_SECRET")
                .unwrap_or(d.session_token_secret),
            session_token_key: std::env::var("CALIBAN_SESSION_TOKEN_KEY")
                .unwrap_or(d.session_token_key),
            session_server_name: std::env::var("CALIBAN_SESSION_SERVER_NAME")
                .unwrap_or(d.session_server_name),
            caliband_cpu_request: optional_quantity(
                std::env::var("CALIBAND_CPU_REQUEST").ok(),
                d.caliband_cpu_request.as_deref(),
            ),
            caliband_memory_request: optional_quantity(
                std::env::var("CALIBAND_MEMORY_REQUEST").ok(),
                d.caliband_memory_request.as_deref(),
            ),
            caliband_cpu_limit: optional_quantity(
                std::env::var("CALIBAND_CPU_LIMIT").ok(),
                d.caliband_cpu_limit.as_deref(),
            ),
            caliband_memory_limit: optional_quantity(
                std::env::var("CALIBAND_MEMORY_LIMIT").ok(),
                d.caliband_memory_limit.as_deref(),
            ),
            cluster_domain: cluster_domain(std::env::var("CALIBAN_CLUSTER_DOMAIN").ok()),
            ingress_pod_selector,
            ingress_namespace,
            env_errors,
        }
    }

    /// Reject settings that would otherwise fail late (#49). A bad port window
    /// surfaced only as a NetworkPolicy the API server rejected on every
    /// reconcile; checking once at startup names the offending variable.
    pub fn validate(&self) -> Result<(), String> {
        if !self.env_errors.is_empty() {
            return Err(self.env_errors.join("; "));
        }
        for (name, port) in [
            ("CALIBAND_PORT", self.caliband_port),
            ("CALIBAN_AGENT_PORT_BASE", self.agent_port_base),
            ("CALIBAN_AGENT_PORT_END", self.agent_port_end),
        ] {
            if !(1..=65535).contains(&port) {
                return Err(format!("{name} {port} is outside 1-65535"));
            }
        }
        if self.agent_port_base > self.agent_port_end {
            return Err(format!(
                "agent port window is empty: CALIBAN_AGENT_PORT_BASE {} > CALIBAN_AGENT_PORT_END {}",
                self.agent_port_base, self.agent_port_end
            ));
        }
        if (self.agent_port_base..=self.agent_port_end).contains(&self.caliband_port) {
            return Err(format!(
                "CALIBAND_PORT {} falls inside the agent port window {}-{}",
                self.caliband_port, self.agent_port_base, self.agent_port_end
            ));
        }
        Ok(())
    }
}

/// Resolve an optional resource quantity from its env value (#50): unset →
/// `default`, a value → that value, and an explicitly empty value → `None`, so
/// an operator can opt out of a default request rather than only override it.
pub fn optional_quantity(value: Option<String>, default: Option<&str>) -> Option<String> {
    match value {
        None => default.map(str::to_string),
        Some(v) if v.trim().is_empty() => None,
        Some(v) => Some(v.trim().to_string()),
    }
}

/// The longest name Kubernetes accepts for a Service (a DNS-1035 label).
const SERVICE_NAME_MAX: usize = 63;

/// Why a task's name can't be provisioned, if it can't (#49). agent-sandbox
/// names the Sandbox's headless Service after the Sandbox, `{task}-sbx`, and a
/// Service name is at most 63 characters, so a longer task name fails every
/// apply. Task names are immutable, so this is permanent for the object.
pub fn task_name_problem(t: &CalibanTask) -> Option<String> {
    let task = t.name_any();
    let service = sandbox_name(t);
    (service.len() > SERVICE_NAME_MAX).then(|| {
        format!(
            "task name '{task}' is {} characters; its Sandbox Service name '{service}' must be at most {SERVICE_NAME_MAX} characters (task names up to {})",
            task.len(),
            SERVICE_NAME_MAX - (service.len() - task.len())
        )
    })
}

/// Parse a `key=value,key2=value2` label selector (#57). Blank yields an empty
/// map (no narrowing). An entry without `=`, or with an empty key or value, is
/// an error rather than being skipped, so a typo can't silently widen or drop
/// the ingress rule.
pub fn parse_selector(raw: &str) -> Result<BTreeMap<String, String>, String> {
    let mut labels = BTreeMap::new();
    for entry in raw.split(',').map(str::trim).filter(|e| !e.is_empty()) {
        match entry.split_once('=') {
            Some((k, v)) if !k.trim().is_empty() && !v.trim().is_empty() => {
                labels.insert(k.trim().to_string(), v.trim().to_string());
            }
            _ => return Err(format!("bad selector entry '{entry}' (want key=value)")),
        }
    }
    Ok(labels)
}

/// Kubernetes' default cluster DNS domain.
const DEFAULT_CLUSTER_DOMAIN: &str = "cluster.local";

/// Resolve the cluster DNS domain from its env value (#54): unset or blank
/// keeps `cluster.local`; surrounding whitespace and a trailing root dot are
/// trimmed, so `corp.internal.` doesn't produce `…svc.corp.internal.`.
pub fn cluster_domain(value: Option<String>) -> String {
    value
        .as_deref()
        .map(|v| v.trim().trim_end_matches('.'))
        .filter(|v| !v.is_empty())
        .unwrap_or(DEFAULT_CLUSTER_DOMAIN)
        .to_string()
}

/// Name of the Sandbox backing a task.
pub fn sandbox_name(t: &CalibanTask) -> String {
    format!("{}-sbx", t.name_any())
}
/// Name of the task's dedicated ServiceAccount.
pub fn sa_name(t: &CalibanTask) -> String {
    format!("{}-sa", t.name_any())
}
/// Name of the task's NetworkPolicy.
pub fn netpol_name(t: &CalibanTask) -> String {
    format!("{}-netpol", t.name_any())
}

/// In-cluster DNS caliband advertises for its per-agent stream endpoints: the
/// Sandbox's headless service FQDN. caliband otherwise advertises its `0.0.0.0`
/// bind address, which prosperod cannot reach (#24). The domain is the
/// cluster's configured DNS domain, not a hardcoded `cluster.local` (#54).
pub fn caliband_advertise_host(t: &CalibanTask, s: &Settings) -> String {
    format!(
        "{}.{}.svc.{}",
        sandbox_name(t),
        t.namespace().unwrap_or_default(),
        s.cluster_domain
    )
}

/// Labels stamped on every child object, keyed to the owning task.
pub fn common_labels(t: &CalibanTask) -> BTreeMap<String, String> {
    BTreeMap::from([
        (
            "app.kubernetes.io/managed-by".to_string(),
            "caliban-operator".to_string(),
        ),
        ("caliban.caliban-ai.dev/task".to_string(), t.name_any()),
    ])
}

/// A controller owner reference to the task, so children cascade-delete.
pub fn owner_ref(t: &CalibanTask) -> OwnerReference {
    OwnerReference {
        api_version: CalibanTask::api_version(&()).to_string(),
        kind: CalibanTask::kind(&()).to_string(),
        name: t.name_any(),
        uid: t.uid().unwrap_or_default(),
        controller: Some(true),
        block_owner_deletion: Some(true),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crd::{CalibanTask, CalibanTaskSpec, TaskSpec, WorkspaceRef};

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
                    permission_posture: None,
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

    #[test]
    fn names_are_deterministic() {
        let t = task();
        assert_eq!(sandbox_name(&t), "refactor-auth-sbx");
        assert_eq!(sa_name(&t), "refactor-auth-sa");
        assert_eq!(netpol_name(&t), "refactor-auth-netpol");
    }

    #[test]
    fn owner_ref_is_controller_with_uid() {
        let o = owner_ref(&task());
        assert_eq!(o.kind, "CalibanTask");
        assert_eq!(o.api_version, "caliban.caliban-ai.dev/v1alpha1");
        assert_eq!(o.name, "refactor-auth");
        assert_eq!(o.uid, "uid-123");
        assert_eq!(o.controller, Some(true));
    }

    #[test]
    fn from_env_defaults_are_neutral() {
        let s = Settings::default();
        assert_eq!(s.caliband_port, 8443);
        assert!(!s.caliband_image.contains("home"));
        assert_eq!(s.workspace_root, "/work");
        assert!(!s.git_image.contains("home"));
        assert!(!s.git_image.is_empty());
    }

    #[test]
    fn session_plane_defaults_match_the_shared_secret_names() {
        let s = Settings::default();
        assert_eq!(s.session_tls_secret, "caliban-session-plane-tls");
        assert_eq!(s.session_token_secret, "caliban-session-plane-token");
        assert_eq!(s.session_token_key, "token");
    }

    /// #32: the CA is only usable if the name being verified matches the
    /// serving cert's SAN. The chart already pins one value
    /// (`global.sessionPlane.serverName`) for both the SAN and the SNI, so the
    /// operator's default must agree with it — `localhost` would never verify.
    #[test]
    fn session_server_name_defaults_to_the_serving_cert_san() {
        assert_eq!(Settings::default().session_server_name, "caliband");
    }

    /// #50: with no resources block every sandbox pod is BestEffort. Requests
    /// by default make it Burstable; limits stay opt-in so a default install
    /// never OOM-kills an agent on a guess.
    #[test]
    fn sandbox_resource_defaults_request_but_do_not_limit() {
        let s = Settings::default();
        assert_eq!(s.caliband_cpu_request.as_deref(), Some("250m"));
        assert_eq!(s.caliband_memory_request.as_deref(), Some("512Mi"));
        assert!(s.caliband_cpu_limit.is_none());
        assert!(s.caliband_memory_limit.is_none());
    }

    #[test]
    fn optional_quantity_env_unset_uses_default_set_overrides_empty_disables() {
        assert_eq!(optional_quantity(None, Some("250m")), Some("250m".into()));
        assert_eq!(
            optional_quantity(Some("1".into()), Some("250m")),
            Some("1".into())
        );
        // An explicitly empty value opts out of the default entirely.
        assert_eq!(optional_quantity(Some(String::new()), Some("250m")), None);
        assert_eq!(optional_quantity(Some("  ".into()), None), None);
        assert_eq!(optional_quantity(None, None), None);
    }

    #[test]
    fn agent_port_defaults_open_the_per_agent_window() {
        let s = Settings::default();
        // caliband draws per-agent stream ports monotonically from this base (#24/#25).
        assert_eq!(s.agent_port_base, 7100);
        // The NetworkPolicy opens [base, end]; 7999 leaves 900 spawns of headroom.
        assert_eq!(s.agent_port_end, 7999);
    }

    #[test]
    fn caliband_advertise_host_is_the_sandbox_service_fqdn() {
        // The routable host prosperod dials for a per-agent stream: the Sandbox's
        // in-cluster service DNS, not caliband's 0.0.0.0 bind address (#24).
        let t = task();
        assert_eq!(
            caliband_advertise_host(&t, &Settings::default()),
            "refactor-auth-sbx.team-a.svc.cluster.local"
        );
    }

    /// #54: clusters provisioned with a non-default DNS domain got an advertise
    /// host that never resolved, so per-agent stream dials failed.
    #[test]
    fn cluster_domain_defaults_to_cluster_local() {
        assert_eq!(Settings::default().cluster_domain, "cluster.local");
    }

    #[test]
    fn advertise_host_uses_the_configured_cluster_domain() {
        let s = Settings {
            cluster_domain: "corp.internal".to_string(),
            ..Settings::default()
        };
        assert_eq!(
            caliband_advertise_host(&task(), &s),
            "refactor-auth-sbx.team-a.svc.corp.internal"
        );
    }

    /// An unset or blank value keeps the default; a fully-qualified value with a
    /// trailing dot must not yield `…svc.corp.internal.` with a doubled root.
    #[test]
    fn cluster_domain_env_value_is_normalised() {
        assert_eq!(cluster_domain(None), "cluster.local");
        assert_eq!(cluster_domain(Some("   ".into())), "cluster.local");
        assert_eq!(
            cluster_domain(Some("corp.internal".into())),
            "corp.internal"
        );
        assert_eq!(
            cluster_domain(Some(" corp.internal. ".into())),
            "corp.internal"
        );
    }

    #[test]
    fn default_settings_are_valid() {
        assert_eq!(Settings::default().validate(), Ok(()));
    }

    /// #49: a bad port window used to surface only as a NetworkPolicy the API
    /// server rejects on every reconcile. Fail once, at startup, instead.
    #[test]
    fn an_empty_agent_port_window_is_rejected() {
        let s = Settings {
            agent_port_base: 8000,
            agent_port_end: 7000,
            ..Settings::default()
        };
        let err = s.validate().unwrap_err();
        assert!(err.contains("CALIBAN_AGENT_PORT_BASE"), "{err}");
    }

    /// The control port inside the agent window would let caliband hand a
    /// per-agent stream the port it is already listening on.
    #[test]
    fn a_control_port_inside_the_agent_window_is_rejected() {
        let s = Settings {
            caliband_port: 7500,
            ..Settings::default()
        };
        let err = s.validate().unwrap_err();
        assert!(err.contains("CALIBAND_PORT"), "{err}");
    }

    #[test]
    fn out_of_range_ports_are_rejected() {
        for s in [
            Settings {
                caliband_port: 0,
                ..Settings::default()
            },
            Settings {
                agent_port_end: 70_000,
                ..Settings::default()
            },
        ] {
            assert!(s.validate().is_err(), "{s:?}");
        }
    }

    /// #49: the Sandbox's headless Service is named `{task}-sbx`, and a Service
    /// name is a DNS-1035 label of at most 63 characters.
    #[test]
    fn a_task_name_that_fits_the_sandbox_service_is_accepted() {
        let mut t = task();
        t.metadata.name = Some("a".repeat(59));
        assert!(task_name_problem(&t).is_none());
    }

    #[test]
    fn a_task_name_too_long_for_the_sandbox_service_is_reported() {
        let mut t = task();
        t.metadata.name = Some("a".repeat(60));
        let msg = task_name_problem(&t).expect("a 60-character name is too long");
        assert!(msg.contains("63"), "{msg}");
    }

    /// #57: the default ingress peer stays exactly what it was — every pod in
    /// the task's own namespace — so existing installs don't lose access.
    #[test]
    fn ingress_defaults_admit_every_pod_in_the_task_namespace() {
        let s = Settings::default();
        assert!(s.ingress_pod_selector.is_empty());
        assert!(s.ingress_namespace.is_none());
        assert!(s.env_errors.is_empty());
    }

    #[test]
    fn ingress_pod_selector_parses_comma_separated_labels() {
        let m = parse_selector("app.kubernetes.io/name=prosperod, tier=control").unwrap();
        assert_eq!(
            m,
            BTreeMap::from([
                (
                    "app.kubernetes.io/name".to_string(),
                    "prosperod".to_string()
                ),
                ("tier".to_string(), "control".to_string()),
            ])
        );
        assert!(parse_selector("").unwrap().is_empty());
        assert!(parse_selector("  ").unwrap().is_empty());
    }

    /// A malformed selector must not silently widen or drop the rule.
    #[test]
    fn a_malformed_ingress_selector_is_rejected() {
        assert!(parse_selector("app").is_err());
        assert!(parse_selector("=prosperod").is_err());
        assert!(parse_selector("app=").is_err());
    }

    /// `from_env` can't fail, so bad env values are recorded and rejected at
    /// startup by `validate`, naming the variable.
    #[test]
    fn validate_reports_env_errors() {
        let s = Settings {
            env_errors: vec!["CALIBAN_INGRESS_POD_SELECTOR: bad entry 'app'".into()],
            ..Settings::default()
        };
        let err = s.validate().unwrap_err();
        assert!(err.contains("CALIBAN_INGRESS_POD_SELECTOR"), "{err}");
    }
}
