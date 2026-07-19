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
        }
    }
}

impl Settings {
    /// Read settings from `CALIBAND_IMAGE`, `CALIBAND_PORT`, `CALIBAN_AGENT_PORT_BASE`,
    /// `CALIBAN_AGENT_PORT_END`, `CALIBAN_WORKSPACE_ROOT`, `CALIBAN_WORKSPACE_STORAGE`,
    /// `CALIBAN_GIT_IMAGE`, `CALIBAN_SESSION_TLS_SECRET`, `CALIBAN_SESSION_TOKEN_SECRET`,
    /// `CALIBAN_SESSION_TOKEN_KEY`, falling back to defaults.
    pub fn from_env() -> Self {
        let d = Self::default();
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
        }
    }
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
/// bind address, which prosperod cannot reach (#24).
pub fn caliband_advertise_host(t: &CalibanTask) -> String {
    format!(
        "{}.{}.svc.cluster.local",
        sandbox_name(t),
        t.namespace().unwrap_or_default()
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
            caliband_advertise_host(&t),
            "refactor-auth-sbx.team-a.svc.cluster.local"
        );
    }
}
