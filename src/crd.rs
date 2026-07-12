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
    /// Reference to the namespace-local `Workspace` this task runs against.
    pub workspace_ref: WorkspaceRef,
    /// Which of the workspace's providers to bind; defaults to the workspace's
    /// `defaultProvider` (or its sole provider).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_ref: Option<String>,
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
    /// Per-run tool override (allow-list) for this task's agents.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<String>>,
}

/// A by-name reference to a `Workspace` in the same namespace.
#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceRef {
    /// Workspace object name.
    #[schemars(length(min = 1))]
    pub name: String,
}

/// A single source checkout in the workspace.
#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Source {
    /// Source identifier (matches caliband's workspace source name).
    #[schemars(length(min = 1))]
    pub name: String,
    /// Git remote to clone.
    #[schemars(length(min = 1))]
    pub repo: String,
    /// Git ref to check out. Defaults to `main`.
    #[serde(default = "default_ref")]
    pub r#ref: String,
    /// Absolute mount path in the pod (e.g. `/work/caliban`).
    #[schemars(length(min = 1))]
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
    #[schemars(length(min = 1))]
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
    // No `skip_serializing_if` here (unlike most optional status fields):
    // `derive_status` transitions this back to `None` when the backing
    // Sandbox disappears, and the status patch uses JSON Merge Patch
    // (RFC 7396), where an *omitted* key is left unchanged on the server but
    // an explicit `null` deletes it. Serializing `None` as `null` is required
    // so a stale endpoint doesn't survive the merge. (`//` not `///` so this
    // doesn't leak into the generated CRD schema description.)
    #[serde(default)]
    pub caliband_endpoint: Option<String>,
    /// The agent-sandbox Sandbox backing this task.
    // See the note on `caliband_endpoint` above: no `skip_serializing_if`,
    // for the same merge-patch-delete-via-null reason.
    #[serde(default)]
    pub sandbox_ref: Option<NamedRef>,
    /// Latest checkpoint reference.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint_ref: Option<String>,
    /// Standard Kubernetes conditions.
    // No `skip_serializing_if` here either (see `caliband_endpoint` above):
    // `derive_status` sets this to `[]` to clear a stale `Ready` condition
    // when the phase leaves `Running`. Under JSON Merge Patch, an *omitted*
    // key is left unchanged server-side, so an empty `Vec` must still
    // serialize as `"conditions": []` — skipping it here would leave the
    // stale condition on the server forever (and cause endless reconcile
    // churn, since the next read would keep disagreeing with what
    // `derive_status` recomputes).
    #[serde(default)]
    pub conditions: Vec<Condition>,
    /// Resolved workspace config, pinned at admission (immutable run). Set once;
    /// later `Workspace` edits don't re-pin a running task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_workspace: Option<crate::workspace::ResolvedWorkspace>,
}

/// A by-name reference to another object in the same namespace.
#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NamedRef {
    /// Object name.
    pub name: String,
}

/// A minimal Kubernetes-style condition.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
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
metadata: { name: refactor-auth, namespace: team-a }
spec:
  workspaceRef: { name: team-a-ws }
  providerRef: planner
  task: { prompt: "refactor the auth module", agentType: general-purpose }
  model:     { routerConfigRef: caliban-router }
  state:     { gonzaloEndpoint: gonzalod.storage.svc, mode: remote }
  isolation: { runtimeClass: gvisor, worktrees: per-source }
  resources: { class: standard }
  lifecycle: { idleTimeout: 30m, onDelete: checkpoint }
"#;

    #[test]
    fn sample_cr_round_trips() {
        let task: CalibanTask = serde_norway::from_str(SAMPLE).expect("deserialize sample");
        assert_eq!(task.spec.workspace_ref.name, "team-a-ws");
        assert_eq!(task.spec.provider_ref.as_deref(), Some("planner"));
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
        assert!(
            json["workspaceRef"]["name"].is_string(),
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
    fn committed_crd_yaml_is_in_sync() {
        let generated = serde_norway::to_string(&CalibanTask::crd()).unwrap();
        let committed = include_str!("../deploy/crd/calibantask.yaml");
        assert_eq!(
            generated.trim(),
            committed.trim(),
            "deploy/crd/calibantask.yaml is stale — regenerate: cargo run --bin crdgen > deploy/crd/calibantask.yaml"
        );
    }

    #[test]
    fn crd_enforces_non_empty_required_fields() {
        // Semantically-invalid CRs (`workspaceRef.name: ""`, `prompt: ""`) must be
        // rejected by the API server, not admitted and set to Pending. The
        // generated CRD schema carries the constraints (schemars `length(min = 1)`).
        // (The `sources` constraint now lives on the Workspace CRD; see workspace.rs.)
        let crd = CalibanTask::crd();
        let schema = serde_json::to_value(&crd.spec.versions[0].schema).unwrap();
        let spec = &schema["openAPIV3Schema"]["properties"]["spec"]["properties"];

        assert_eq!(
            spec["workspaceRef"]["properties"]["name"]["minLength"], 1,
            "workspaceRef.name must require minLength: 1"
        );
        assert_eq!(
            spec["task"]["properties"]["prompt"]["minLength"], 1,
            "task.prompt must require minLength: 1"
        );
    }

    #[test]
    fn minimal_cr_defaults() {
        // Only required fields; optional blocks absent.
        let yaml = r#"
apiVersion: caliban.caliban-ai.dev/v1alpha1
kind: CalibanTask
metadata: { name: m, namespace: n }
spec:
  workspaceRef: { name: only-ws }
  task: { prompt: hi }
"#;
        let task: CalibanTask = serde_norway::from_str(yaml).unwrap();
        assert_eq!(task.spec.workspace_ref.name, "only-ws");
        assert!(task.spec.provider_ref.is_none());
        assert!(task.spec.model.is_none());
        assert!(task.spec.task.agent_type.is_none());
        assert!(task.spec.tools.is_none());
    }
}
