//! The `CalibanTask` custom resource (v1alpha1). Mirrors the k8s system-design
//! spec's CR. See ADR 0001.

use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Desired state of a caliban task: a workspace of sources + the task to run.
#[derive(CustomResource, Serialize, Deserialize, Clone, Debug, JsonSchema)]
#[kube(
    group = "caliban.caliban-ai.dev",
    version = "v1alpha1",
    kind = "CalibanTask",
    namespaced,
    status = "CalibanTaskStatus",
    shortname = "ctask",
    printcolumn = r#"{"name":"Phase","type":"string","jsonPath":".status.phase"}"#,
    printcolumn = r#"{"name":"Age","type":"date","jsonPath":".metadata.creationTimestamp"}"#
)]
#[serde(rename_all = "camelCase")]
pub struct CalibanTaskSpec {
    /// The workspace (1..N source checkouts) the task runs over.
    pub workspace: Workspace,
    /// The task itself.
    pub task: TaskSpec,
    /// Model-router configuration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelSpec>,
    /// Persistence (gonzalo) configuration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<StateSpec>,
    /// Sandbox isolation configuration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub isolation: Option<IsolationSpec>,
    /// Resource class → a SandboxTemplate (consumed in #283).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resources: Option<ResourcesSpec>,
    /// Idle/drain lifecycle policy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lifecycle: Option<LifecycleSpec>,
}

/// A workspace: the provisioned source set + optional in-pod aux services.
#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Workspace {
    /// The guaranteed source checkouts (runtime-extensible).
    pub sources: Vec<Source>,
    /// Optional in-pod aux services (e.g. gonzalod, prosperod) for e2e.
    #[serde(default)]
    pub services: Vec<String>,
}

/// A single source checkout in the workspace.
#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Source {
    /// Source identifier (matches caliband's workspace source name).
    pub name: String,
    /// Git remote to clone.
    pub repo: String,
    /// Git ref to check out. Defaults to `main`.
    #[serde(default = "default_ref")]
    pub r#ref: String,
    /// Absolute mount path in the pod (e.g. `/work/caliban`).
    pub path: String,
}

fn default_ref() -> String {
    "main".to_string()
}

/// The task to run in the workspace.
#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TaskSpec {
    /// Initial prompt.
    pub prompt: String,
    /// Agent type (e.g. `general-purpose`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_type: Option<String>,
}

/// Model-router configuration.
#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ModelSpec {
    /// Name of a ConfigMap holding the router config.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub router_config_ref: Option<String>,
}

/// Persistence (gonzalo) configuration.
#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct StateSpec {
    /// gonzalod endpoint the pod uses for shared state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gonzalo_endpoint: Option<String>,
    /// `remote` (shared gonzalod) or `local` (in-pod).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
}

/// Sandbox isolation configuration.
#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct IsolationSpec {
    /// RuntimeClass (e.g. `gvisor`, `kata`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_class: Option<String>,
    /// Worktree isolation strategy (e.g. `per-source`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktrees: Option<String>,
}

/// Resource class selecting a SandboxTemplate.
#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ResourcesSpec {
    /// Named resource class (e.g. `standard`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub class: Option<String>,
}

/// Idle/drain lifecycle policy.
#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct LifecycleSpec {
    /// Idle timeout before pause (e.g. `30m`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_timeout: Option<String>,
    /// On delete: `checkpoint` or `delete`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_delete: Option<String>,
}

/// Observed state of a `CalibanTask`.
#[derive(Serialize, Deserialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CalibanTaskStatus {
    /// Lifecycle phase.
    #[serde(default)]
    pub phase: Phase,
    /// caliband session endpoint (host:port), once the Sandbox is ready.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caliband_endpoint: Option<String>,
    /// The agent-sandbox Sandbox backing this task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox_ref: Option<NamedRef>,
    /// Latest checkpoint reference.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint_ref: Option<String>,
    /// Observed workspace (incl. runtime-added sources).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<WorkspaceStatus>,
    /// Standard Kubernetes conditions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<Condition>,
}

/// A by-name reference to another object in the same namespace.
#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NamedRef {
    /// Object name.
    pub name: String,
}

/// Observed workspace state.
#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceStatus {
    /// Source names observed materialized in the pod.
    #[serde(default)]
    pub materialized: Vec<String>,
}

/// A minimal Kubernetes-style condition.
#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Condition {
    /// Condition type (e.g. `Ready`).
    #[serde(rename = "type")]
    pub type_: String,
    /// `True` / `False` / `Unknown`.
    pub status: String,
    /// Machine-readable reason.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Human-readable message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// `CalibanTask` lifecycle phase.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq, JsonSchema)]
pub enum Phase {
    /// Accepted, not yet provisioning.
    #[default]
    Pending,
    /// Sandbox/objects being created.
    Provisioning,
    /// caliband is up and attachable.
    Running,
    /// Draining/checkpointing before teardown.
    Draining,
    /// Finished successfully.
    Completed,
    /// Finished with an error.
    Failed,
}

#[cfg(test)]
mod tests {
    use super::*;
    use kube::CustomResourceExt;

    // The design spec's sample CalibanTask (camelCase YAML).
    const SAMPLE: &str = r#"
apiVersion: caliban.caliban-ai.dev/v1alpha1
kind: CalibanTask
metadata:
  name: refactor-auth
  namespace: team-a
spec:
  workspace:
    sources:
      - { name: caliban,  repo: "git@example:caliban",  ref: main,       path: /work/caliban }
      - { name: prospero, repo: "git@example:prospero", ref: feat-xport, path: /work/prospero }
    services: [ gonzalod, prosperod ]
  task:      { prompt: "refactor the auth module", agentType: general-purpose }
  model:     { routerConfigRef: caliban-router }
  state:     { gonzaloEndpoint: gonzalod.storage.svc, mode: remote }
  isolation: { runtimeClass: gvisor, worktrees: per-source }
  resources: { class: standard }
  lifecycle: { idleTimeout: 30m, onDelete: checkpoint }
"#;

    #[test]
    fn sample_cr_round_trips() {
        let task: CalibanTask = serde_yaml::from_str(SAMPLE).expect("deserialize sample");
        assert_eq!(task.spec.workspace.sources.len(), 2);
        assert_eq!(task.spec.workspace.sources[0].name, "caliban");
        assert_eq!(task.spec.workspace.sources[1].r#ref, "feat-xport");
        assert_eq!(task.spec.workspace.services, vec!["gonzalod", "prosperod"]);
        assert_eq!(task.spec.task.prompt, "refactor the auth module");
        assert_eq!(
            task.spec.task.agent_type.as_deref(),
            Some("general-purpose")
        );
        assert_eq!(
            task.spec
                .model
                .as_ref()
                .and_then(|m| m.router_config_ref.as_deref()),
            Some("caliban-router")
        );
        assert_eq!(
            task.spec
                .isolation
                .as_ref()
                .and_then(|i| i.worktrees.as_deref()),
            Some("per-source")
        );
        // Re-serialize spec and confirm camelCase keys survive.
        let json = serde_json::to_value(&task.spec).unwrap();
        assert!(
            json["task"]["agentType"].is_string(),
            "camelCase key expected"
        );
    }

    #[test]
    fn crd_has_correct_group_version_kind() {
        let crd = CalibanTask::crd();
        assert_eq!(crd.spec.group, "caliban.caliban-ai.dev");
        assert_eq!(crd.spec.names.kind, "CalibanTask");
        assert_eq!(crd.spec.versions[0].name, "v1alpha1");
        assert_eq!(crd.spec.scope, "Namespaced");
        assert!(crd.spec.versions[0]
            .subresources
            .as_ref()
            .unwrap()
            .status
            .is_some());
    }

    #[test]
    fn minimal_cr_defaults() {
        // Only required fields; optional blocks absent.
        let yaml = r#"
apiVersion: caliban.caliban-ai.dev/v1alpha1
kind: CalibanTask
metadata: { name: m, namespace: n }
spec:
  workspace: { sources: [ { name: only, repo: "git@x:only", path: /work/only } ] }
  task: { prompt: hi }
"#;
        let task: CalibanTask = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(task.spec.workspace.sources[0].r#ref, "main"); // default ref
        assert!(task.spec.workspace.services.is_empty());
        assert!(task.spec.model.is_none());
        assert!(task.spec.task.agent_type.is_none());
    }
}
