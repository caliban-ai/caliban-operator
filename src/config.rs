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
    /// Workspace root mount path in the pod.
    pub workspace_root: String,
    /// Requested size of the workspace PVC (e.g. `10Gi`).
    pub workspace_storage: String,
    /// Container image for the git-clone init container that populates the workspace.
    pub git_image: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            caliband_image: "ghcr.io/caliban-ai/caliban:latest".to_string(),
            caliband_port: 8443,
            workspace_root: "/work".to_string(),
            workspace_storage: "10Gi".to_string(),
            git_image: "alpine/git:latest".to_string(),
        }
    }
}

impl Settings {
    /// Read settings from `CALIBAND_IMAGE`, `CALIBAND_PORT`, `CALIBAN_WORKSPACE_ROOT`,
    /// `CALIBAN_WORKSPACE_STORAGE`, `CALIBAN_GIT_IMAGE`, falling back to defaults.
    pub fn from_env() -> Self {
        let d = Self::default();
        Self {
            caliband_image: std::env::var("CALIBAND_IMAGE").unwrap_or(d.caliband_image),
            caliband_port: std::env::var("CALIBAND_PORT")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(d.caliband_port),
            workspace_root: std::env::var("CALIBAN_WORKSPACE_ROOT").unwrap_or(d.workspace_root),
            workspace_storage: std::env::var("CALIBAN_WORKSPACE_STORAGE")
                .unwrap_or(d.workspace_storage),
            git_image: std::env::var("CALIBAN_GIT_IMAGE").unwrap_or(d.git_image),
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
}
