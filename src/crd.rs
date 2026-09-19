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
    printcolumn = r#"{"name":"Posture","type":"string","jsonPath":".status.permissionPosture"}"#,
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
    /// Run the agent in interactive mode: it awaits operator input at each
    /// end-of-run instead of finishing. The operator only carries this through
    /// to the CR — prospero reads it back and sets `SpawnSpec.interactive` on
    /// the pod caliband itself (prospero#163).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interactive: Option<bool>,
    /// How the task's agents handle a permission prompt: `supervised` (the
    /// default) sends it to a human to answer; `unattended` runs under a bypass
    /// profile and is only admitted when the Workspace's `agentPolicy` sets
    /// `allowUnattended`. Unset means `supervised`.
    // prospero maps this onto `SpawnSpec.permission_posture` (caliban ADR 0059,
    // caliban#676); the operator never hands it to caliband itself (ADR 0005).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_posture: Option<PermissionPosture>,
}

// A session's permission posture (caliban ADR 0059). `//`, not `///`: an enum's
// doc replaces the referencing field's description in the generated CRD.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum PermissionPosture {
    /// A permission prompt goes to a human, who answers it.
    #[default]
    Supervised,
    /// The session runs under an authorized bypass profile.
    Unattended,
}

impl TaskSpec {
    /// The posture this task asks for: `supervised` when unset (fail-closed).
    pub fn posture(&self) -> PermissionPosture {
        self.permission_posture.unwrap_or_default()
    }
}

/// Model-router configuration.
#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ModelSpec {
    /// Name of a ConfigMap (same namespace) holding the model router config
    /// under the key `caliban.toml`. Mounted read-only into the sandbox and
    /// handed to caliban as `CALIBAN_ROUTER_CONFIG`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub router_config_ref: Option<String>,
}

/// Persistence (gonzalo) configuration.
#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct StateSpec {
    /// gonzalod URL the task's agents use for shared state (e.g.
    /// `http://gonzalod.storage.svc:8080`). Setting it selects remote storage
    /// unless `mode` says `local`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gonzalo_endpoint: Option<String>,
    /// `remote` (shared gonzalod) or `local` (in-pod filesystem). Defaults to
    /// `remote` when `gonzaloEndpoint` is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// Secret holding the gonzalod bearer token, projected into the pod as
    /// `GONZALO_TOKEN`. Requires remote storage. caliban-memory and
    /// caliban-sessions write to the gonzalod `caliban` namespace, so use a
    /// principal with read/write on `caliban` only and no write on `fleet` or
    /// `fleet-audit`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_ref: Option<crate::workspace::CredentialsRef>,
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
    // Status is written by server-side apply (#64, ADR 0005): a field the
    // operator's manager owned but omits from its next apply is removed, so a
    // cleared value is *omitted*, never serialized as `null`. (`//` not `///`
    // so this doesn't leak into the generated CRD schema description.)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caliband_endpoint: Option<String>,
    /// The agent-sandbox Sandbox backing this task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox_ref: Option<NamedRef>,
    /// Latest checkpoint reference.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint_ref: Option<String>,
    /// Standard Kubernetes conditions.
    // A map-list keyed by `type`, so each field manager owns only its own
    // entries (ADR 0005): the operator applies `Ready`, prospero applies
    // `AgentsSettled`, and neither apply replaces the other's condition.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[schemars(extend(
        "x-kubernetes-list-type" = "map",
        "x-kubernetes-list-map-keys" = ["type"]
    ))]
    pub conditions: Vec<Condition>,
    /// Resolved workspace config, pinned at admission (immutable run). Set once;
    /// later `Workspace` edits don't re-pin a running task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_workspace: Option<crate::workspace::ResolvedWorkspace>,
    /// The permission posture the task was admitted with, so an unattended
    /// session is visible from `kubectl get`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_posture: Option<PermissionPosture>,
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
  state:     { gonzaloEndpoint: "http://gonzalod.storage.svc:8080", mode: remote, tokenRef: { secretName: gonzalo-agent-token, key: token } }
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
    fn crd_schema_exposes_task_interactive_as_boolean() {
        // prospero projects `SpawnRequest.interactive` onto this field and reads
        // it back in `spawn_spec_from_task`; without it the flag is dropped at
        // the CR boundary and a k8s agent can never await input (prospero#163).
        let crd = CalibanTask::crd();
        let schema = serde_json::to_value(&crd.spec.versions[0].schema).unwrap();
        let task = &schema["openAPIV3Schema"]["properties"]["spec"]["properties"]["task"];

        assert_eq!(
            task["properties"]["interactive"]["type"], "boolean",
            "task.interactive must be a boolean in the generated schema"
        );
        assert!(
            !task["required"]
                .as_array()
                .is_some_and(|r| r.iter().any(|f| f == "interactive")),
            "task.interactive must stay optional so existing CRs remain valid"
        );
    }

    #[test]
    fn cr_without_interactive_deserializes_to_none() {
        // Back-compat: every CalibanTask written before this field existed must
        // still apply unchanged.
        let yaml = r#"
apiVersion: caliban.caliban-ai.dev/v1alpha1
kind: CalibanTask
metadata: { name: m, namespace: n }
spec:
  workspaceRef: { name: only-ws }
  task: { prompt: hi }
"#;
        let task: CalibanTask = serde_norway::from_str(yaml).unwrap();
        assert!(
            task.spec.task.interactive.is_none(),
            "an absent interactive field must deserialize to None, not error"
        );
    }

    #[test]
    fn cr_with_interactive_true_round_trips() {
        let yaml = r#"
apiVersion: caliban.caliban-ai.dev/v1alpha1
kind: CalibanTask
metadata: { name: m, namespace: n }
spec:
  workspaceRef: { name: only-ws }
  task: { prompt: hi, interactive: true }
"#;
        let task: CalibanTask = serde_norway::from_str(yaml).unwrap();
        assert_eq!(task.spec.task.interactive, Some(true));

        // camelCase key survives re-serialization (prospero reads it back).
        let json = serde_json::to_value(&task.spec).unwrap();
        assert_eq!(json["task"]["interactive"], serde_json::json!(true));
    }

    #[test]
    fn crd_schema_restricts_permission_posture_to_its_two_values() {
        // caliban ADR 0059: the API server must reject anything but the two
        // postures, so a typo can never reach prospero as an unknown posture.
        let crd = CalibanTask::crd();
        let schema = serde_json::to_value(&crd.spec.versions[0].schema).unwrap();
        let task = &schema["openAPIV3Schema"]["properties"]["spec"]["properties"]["task"];
        let posture = &task["properties"]["permissionPosture"];
        assert_eq!(
            posture["enum"],
            serde_json::json!(["supervised", "unattended"]),
            "{posture}"
        );
        assert!(
            !task["required"]
                .as_array()
                .is_some_and(|r| r.iter().any(|f| f == "permissionPosture")),
            "permissionPosture must stay optional so existing CRs remain valid"
        );
    }

    #[test]
    fn an_unset_posture_is_supervised() {
        let yaml = r#"
apiVersion: caliban.caliban-ai.dev/v1alpha1
kind: CalibanTask
metadata: { name: m, namespace: n }
spec:
  workspaceRef: { name: only-ws }
  task: { prompt: hi }
"#;
        let task: CalibanTask = serde_norway::from_str(yaml).unwrap();
        assert!(task.spec.task.permission_posture.is_none());
        assert_eq!(task.spec.task.posture(), PermissionPosture::Supervised);
    }

    #[test]
    fn an_unattended_posture_round_trips_in_camel_case() {
        let yaml = r#"
apiVersion: caliban.caliban-ai.dev/v1alpha1
kind: CalibanTask
metadata: { name: m, namespace: n }
spec:
  workspaceRef: { name: only-ws }
  task: { prompt: hi, permissionPosture: unattended }
"#;
        let task: CalibanTask = serde_norway::from_str(yaml).unwrap();
        assert_eq!(task.spec.task.posture(), PermissionPosture::Unattended);
        // prospero reads the field back by this exact key and value.
        let json = serde_json::to_value(&task.spec).unwrap();
        assert_eq!(json["task"]["permissionPosture"], "unattended");
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
