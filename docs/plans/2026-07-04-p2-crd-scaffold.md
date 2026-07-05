# caliban-operator: kube-rs scaffold + CalibanTask CRD Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Scaffold the kube-rs operator and define the `CalibanTask` v1alpha1 CRD, with a generated CRD YAML and a controller skeleton whose reconcile is a no-op that sets `.status.phase = Pending`.

**Architecture:** A single `caliban-operator` binary built on kube-rs. `CalibanTaskSpec`/`CalibanTaskStatus` (via `#[derive(CustomResource)]`) mirror the design spec's CR; a `crdgen` binary emits the CRD YAML (committed + kept in sync by a test); the controller watches `CalibanTask` and its reconcile only initializes status. Real reconcile (→ agent-sandbox `Sandbox`) is #283. See ADR 0001.

**Tech Stack:** Rust 1.95, `kube` (runtime/derive/client), `k8s-openapi` (v1_32), `schemars`, tokio, serde, serde_yaml, thiserror, tracing, futures.

## Global Constraints

- **ADR 0001 governs:** API group `caliban.caliban-ai.dev`, version `v1alpha1`, kind `CalibanTask`, **namespaced**, **status subresource**. `v1alpha1` = unstable API, no conversion webhooks.
- **CRD YAML is generated from the Rust types** (crdgen), committed, and a test asserts the committed file is in sync (regenerate-and-compare) — the Rust struct is the single source of truth.
- **kube/k8s-openapi versions are resolved with `cargo add`** (which honors kube's k8s-openapi pairing), not hand-picked; pin whatever resolves. `k8s-openapi` feature `v1_32`.
- **Field naming:** YAML is camelCase (k8s convention); Rust structs are snake_case with `#[serde(rename_all = "camelCase")]`. `ref` is a Rust keyword → `#[serde(rename = "ref")] r#ref` or a renamed field.
- **#282 creates NO Kubernetes objects.** The reconcile only sets status. No Sandbox/RBAC/NetworkPolicy (that's #283).
- CI is the standard gate: `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo build --workspace --all-targets`, `cargo test --workspace`. No `unwrap()`/`expect()` in non-test code.
- The **acceptance** ("operator runs, watches, a submitted CR gets status Pending") is a *live-cluster* behavior validated at deploy/QA time; the CI-testable portion is: the types round-trip against the spec sample, the generated CRD has the right group/version/kind and is in sync, and the reconcile decision sets Pending. Do not attempt a live-cluster integration test in CI (no cluster available).

---

## File Structure

- **Modify** `Cargo.toml` — deps + two `[[bin]]` targets (`caliban-operator`, `crdgen`); bump `version` to `0.1.0`.
- **Create** `src/lib.rs` (replace the scaffold doc) — `pub mod crd; pub mod controller;` + re-exports.
- **Create** `src/crd.rs` — `CalibanTask` (`CustomResource`), all spec/status sub-types, `Phase` enum.
- **Create** `src/controller.rs` — the reconcile fn (no-op → set Pending), the error type, the `run()` that wires `kube::runtime::Controller`.
- **Create** `src/bin/caliban-operator.rs` — main: build client, call `controller::run`.
- **Create** `src/bin/crdgen.rs` — print `CalibanTask::crd()` as YAML.
- **Create** `deploy/crd/calibantask.yaml` — the committed generated CRD.
- **Test:** inline `#[cfg(test)]` in `crd.rs` (serde round-trip, crd() metadata) and `controller.rs` (reconcile decision); a sync test that regenerates the CRD and compares to the committed file.

---

### Task 1: kube-rs scaffold — deps + a binary that builds and connects

**Files:**
- Modify: `Cargo.toml`
- Create: `src/lib.rs` (replace scaffold), `src/bin/caliban-operator.rs`

**Interfaces:**
- Produces: a compiling `caliban-operator` binary that (in `main`) constructs a `kube::Client` via `Client::try_default()` and logs a startup line, then exits (the controller wiring lands in Task 4). `lib.rs` declares the module structure.

- [ ] **Step 1: Add deps with `cargo add`** (resolves compatible versions):

```bash
cargo add kube --features runtime,derive,client
cargo add k8s-openapi --features v1_32 --no-default-features
cargo add schemars
cargo add tokio --features macros,rt-multi-thread
cargo add serde --features derive
cargo add serde_json
cargo add serde_yaml
cargo add thiserror
cargo add tracing
cargo add tracing-subscriber --features env-filter
cargo add futures
cargo add anyhow
# dev-deps for tests:
cargo add --dev pretty_assertions
```
Bump `version = "0.1.0"` and add the two bin targets:
```toml
[[bin]]
name = "caliban-operator"
path = "src/bin/caliban-operator.rs"

[[bin]]
name = "crdgen"
path = "src/bin/crdgen.rs"
```
Add `[lints]` if the sibling repos use a shared lint table; otherwise rely on the CI `-D warnings` flag.

- [ ] **Step 2: `src/lib.rs`** (replace the scaffold doc):

```rust
//! caliban-operator — a kube-rs operator that reconciles `CalibanTask` custom
//! resources into sandboxed caliband pods (via agent-sandbox). See ADR 0001 and
//! the k8s system-design spec (epic caliban-ai/caliban#274).

pub mod controller;
pub mod crd;

pub use crd::{CalibanTask, CalibanTaskSpec, CalibanTaskStatus, Phase};
```

(Until Tasks 2–4 create `crd`/`controller`, this won't compile — so in Task 1 create minimal placeholder modules: `src/crd.rs` with `// filled in Task 2` and `src/controller.rs` with `// filled in Task 4`, each empty but present, and have `lib.rs` declare them. Or defer the `pub use` to Task 2. Prefer: create empty `crd.rs`/`controller.rs` now, no re-exports yet; add re-exports in Task 2/4.)

- [ ] **Step 3: `src/bin/caliban-operator.rs`**

```rust
//! caliban-operator entrypoint.

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    tracing::info!("caliban-operator starting");
    let _client = kube::Client::try_default().await?;
    tracing::info!("connected to the Kubernetes API");
    // Controller wiring lands in Task 4.
    Ok(())
}
```

- [ ] **Step 4: Build** — `cargo build --workspace --all-targets`. Expected: compiles. (It won't run without a cluster; that's fine — Task 1's deliverable is a compiling stack.) Run `cargo fmt --all -- --check` and `cargo clippy --workspace --all-targets -- -D warnings`.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat(operator): kube-rs scaffold — deps + compiling binary (#282)"
```

---

### Task 2: `CalibanTask` CRD types (v1alpha1)

**Files:**
- Create/replace: `src/crd.rs`
- Modify: `src/lib.rs` (add the `pub use crd::...` re-exports)
- Test: inline `#[cfg(test)]`

**Interfaces:**
- Produces the CRD types below (exact names — Task 4 + #283 consume them). All derive `Serialize, Deserialize, Clone, Debug, JsonSchema` and use `#[serde(rename_all = "camelCase")]`.

- [ ] **Step 1: Write the failing test** (round-trips the spec's sample CR + asserts `crd()` metadata)

```rust
#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(task.spec.task.agent_type.as_deref(), Some("general-purpose"));
        assert_eq!(task.spec.model.as_ref().and_then(|m| m.router_config_ref.as_deref()), Some("caliban-router"));
        assert_eq!(task.spec.isolation.as_ref().and_then(|i| i.worktrees.as_deref()), Some("per-source"));
        // Re-serialize spec and confirm camelCase keys survive.
        let json = serde_json::to_value(&task.spec).unwrap();
        assert!(json["task"]["agentType"].is_string(), "camelCase key expected");
    }

    #[test]
    fn crd_has_correct_group_version_kind() {
        let crd = CalibanTask::crd();
        assert_eq!(crd.spec.group, "caliban.caliban-ai.dev");
        assert_eq!(crd.spec.names.kind, "CalibanTask");
        assert_eq!(crd.spec.versions[0].name, "v1alpha1");
        assert_eq!(crd.spec.scope, "Namespaced");
        assert!(crd.spec.versions[0].subresources.as_ref().unwrap().status.is_some());
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
```

- [ ] **Step 2: Run to verify it fails** — `cargo test -p caliban-operator crd::tests` → FAIL (types missing).

- [ ] **Step 3: Implement `src/crd.rs`**

```rust
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
```

Add to `lib.rs`:
```rust
pub use crd::{CalibanTask, CalibanTaskSpec, CalibanTaskStatus, Phase};
```

- [ ] **Step 4: Run to verify it passes** — `cargo test -p caliban-operator crd::tests`. If `serde_yaml` flow-map parsing of the `{ name: caliban, ... }` inline maps trips, adjust the SAMPLE to block style; keep the assertions. If `CalibanTask::crd()` requires the `kube` `derive` + a `.crd()` method (it does via `CustomResourceExt`), `use kube::CustomResourceExt;` in the test.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat(operator): CalibanTask v1alpha1 CRD types (#282)"
```

---

### Task 3: `crdgen` binary + committed CRD YAML + in-sync test

**Files:**
- Create: `src/bin/crdgen.rs`, `deploy/crd/calibantask.yaml`
- Test: inline sync test (in `crd.rs` or a `tests/` file)

**Interfaces:**
- Consumes: `CalibanTask::crd()`.
- Produces: `crdgen` prints the CRD YAML to stdout; `deploy/crd/calibantask.yaml` is the committed output; a test regenerates and compares.

- [ ] **Step 1: `src/bin/crdgen.rs`**

```rust
//! Emit the CalibanTask CRD YAML: `cargo run --bin crdgen > deploy/crd/calibantask.yaml`.

use kube::CustomResourceExt;

fn main() -> anyhow::Result<()> {
    let crd = caliban_operator::crd::CalibanTask::crd();
    print!("{}", serde_yaml::to_string(&crd)?);
    Ok(())
}
```

- [ ] **Step 2: Generate + commit the CRD**

```bash
cargo run --bin crdgen > deploy/crd/calibantask.yaml
```
Sanity-check it starts with `apiVersion: apiextensions.k8s.io/v1` and `kind: CustomResourceDefinition`, `name: calibantasks.caliban.caliban-ai.dev`.

- [ ] **Step 3: Write the in-sync test** (so the committed YAML can't drift from the types):

```rust
// in crd.rs tests (or tests/crd_sync.rs)
#[test]
fn committed_crd_yaml_is_in_sync() {
    use kube::CustomResourceExt;
    let generated = serde_yaml::to_string(&CalibanTask::crd()).unwrap();
    let committed = include_str!("../deploy/crd/calibantask.yaml"); // adjust path
    assert_eq!(
        generated.trim(),
        committed.trim(),
        "deploy/crd/calibantask.yaml is stale — regenerate: cargo run --bin crdgen > deploy/crd/calibantask.yaml"
    );
}
```

(Use the correct relative path for `include_str!` from the test file's location. If `crd.rs` is at `src/crd.rs`, the path to the repo-root `deploy/…` is `../deploy/crd/calibantask.yaml`.)

- [ ] **Step 4: Run** — `cargo test -p caliban-operator committed_crd_yaml_is_in_sync` → PASS. Then `cargo run --bin crdgen | diff - deploy/crd/calibantask.yaml` → no diff.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat(operator): crdgen binary + committed CalibanTask CRD YAML + sync test (#282)"
```

---

### Task 4: Controller skeleton — no-op reconcile sets `.status.phase = Pending`

**Files:**
- Create/replace: `src/controller.rs`
- Modify: `src/bin/caliban-operator.rs` (call `controller::run`)
- Test: inline reconcile-decision test

**Interfaces:**
- Consumes: `CalibanTask`, `CalibanTaskStatus`, `Phase`.
- Produces: `pub async fn run(client: kube::Client) -> anyhow::Result<()>` wiring a `kube::runtime::Controller<CalibanTask>`; `async fn reconcile(obj, ctx) -> Result<Action, Error>` that patches status to `Pending` if unset and requeues; an `error_policy`.

- [ ] **Step 1: Write the failing test** — a pure decision test for the reconcile's status choice, without a live cluster. Factor the decision into a pure fn:

```rust
// the pure decision the reconcile makes, unit-testable without a cluster
pub(crate) fn desired_status(current: Option<&CalibanTaskStatus>) -> Option<CalibanTaskStatus> {
    // #282: if there's no status yet (or phase is unset/default), initialize to Pending.
    match current {
        Some(s) if s.phase != Phase::Pending || s.caliband_endpoint.is_some() => None, // nothing to do (already progressed)
        _ => Some(CalibanTaskStatus { phase: Phase::Pending, ..Default::default() }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn initializes_missing_status_to_pending() {
        let d = desired_status(None).expect("should set status");
        assert_eq!(d.phase, Phase::Pending);
    }
    #[test]
    fn leaves_progressed_status_untouched() {
        let running = CalibanTaskStatus { phase: Phase::Running, ..Default::default() };
        assert!(desired_status(Some(&running)).is_none());
    }
}
```

- [ ] **Step 2: Run to verify it fails** — `cargo test -p caliban-operator controller::tests` → FAIL.

- [ ] **Step 3: Implement `src/controller.rs`**

```rust
//! CalibanTask controller. #282: a no-op reconcile that initializes status to
//! Pending. The real reconcile (→ agent-sandbox Sandbox + RBAC/NetworkPolicy) is
//! caliban-ai/caliban#283.

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt as _;
use kube::api::{Patch, PatchParams};
use kube::runtime::controller::Action;
use kube::runtime::watcher::Config;
use kube::runtime::Controller;
use kube::{Api, Client, ResourceExt};

use crate::crd::{CalibanTask, CalibanTaskStatus, Phase};

/// Controller error.
#[derive(thiserror::Error, Debug)]
pub enum Error {
    /// Kubernetes API error.
    #[error("kube api: {0}")]
    Kube(#[from] kube::Error),
    /// Status serialization error.
    #[error("serialize status: {0}")]
    Serialize(#[from] serde_json::Error),
}

/// Shared reconcile context.
pub struct Context {
    /// Kubernetes client.
    pub client: Client,
}

/// The pure status decision (unit-testable; see tests).
pub(crate) fn desired_status(current: Option<&CalibanTaskStatus>) -> Option<CalibanTaskStatus> {
    match current {
        Some(s) if s.phase != Phase::Pending || s.caliband_endpoint.is_some() => None,
        _ => Some(CalibanTaskStatus { phase: Phase::Pending, ..Default::default() }),
    }
}

async fn reconcile(obj: Arc<CalibanTask>, ctx: Arc<Context>) -> Result<Action, Error> {
    let ns = obj.namespace().unwrap_or_default();
    let name = obj.name_any();
    let api: Api<CalibanTask> = Api::namespaced(ctx.client.clone(), &ns);

    if let Some(status) = desired_status(obj.status.as_ref()) {
        let patch = serde_json::json!({ "status": status });
        api.patch_status(&name, &PatchParams::default(), &Patch::Merge(&patch)).await?;
        tracing::info!(%ns, %name, "initialized CalibanTask status to Pending");
    }
    // #282 does nothing else; requeue on a slow cadence.
    Ok(Action::requeue(Duration::from_secs(300)))
}

fn error_policy(_obj: Arc<CalibanTask>, err: &Error, _ctx: Arc<Context>) -> Action {
    tracing::warn!(error = %err, "reconcile error");
    Action::requeue(Duration::from_secs(30))
}

/// Run the CalibanTask controller until shutdown.
pub async fn run(client: Client) -> anyhow::Result<()> {
    let tasks: Api<CalibanTask> = Api::all(client.clone());
    let ctx = Arc::new(Context { client });
    Controller::new(tasks, Config::default())
        .run(reconcile, error_policy, ctx)
        .for_each(|res| async move {
            match res {
                Ok((obj, _action)) => tracing::debug!(?obj, "reconciled"),
                Err(e) => tracing::warn!(error = %e, "controller error"),
            }
        })
        .await;
    Ok(())
}
```

Update `src/bin/caliban-operator.rs` `main` to call it:
```rust
    let client = kube::Client::try_default().await?;
    tracing::info!("connected to the Kubernetes API");
    caliban_operator::controller::run(client).await
```

- [ ] **Step 4: Run tests + full gate** — `cargo test -p caliban-operator` (the decision tests pass; note the live controller path is not exercised in CI). Then `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo build --workspace --all-targets`.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat(operator): CalibanTask controller skeleton — reconcile sets status Pending (#282)"
```

---

## Self-Review

**1. Spec coverage** (#282 acceptance + ADR 0001):
- kube-rs scaffold → Task 1. CRD `CalibanTask` v1alpha1 with all spec fields + status → Task 2. CRD YAML generation → Task 3. Controller skeleton + no-op reconcile setting Pending → Task 4. ✓
- "CRD installs" → the committed `deploy/crd/calibantask.yaml` (Task 3) is `kubectl apply`-able; the #284 chart packages it. "operator runs + watches + CR→Pending" → Task 4 wires the Controller + sets Pending; the *live* behavior is QA-validated at deploy (no cluster in CI). ✓
- API group/version/kind/namespaced/status subresource (ADR 0001) → asserted by `crd_has_correct_group_version_kind` (Task 2). ✓

**2. Placeholder scan:** No TBD/vague steps. Task 1 notes the compatible-version resolution is via `cargo add` (exact versions can't be pinned in-plan without resolving them) — that's a grounded instruction, not a placeholder; the implementer pins what resolves.

**3. Type consistency:** `CalibanTask`/`CalibanTaskSpec`/`CalibanTaskStatus`/`Phase` (Task 2) consumed by `crdgen` (Task 3) and `controller` (Task 4). `desired_status(Option<&CalibanTaskStatus>) -> Option<CalibanTaskStatus>` defined + tested + called consistently in Task 4.

**Carry-overs to flag in the whole-branch review:** (a) the live "operator watches + sets Pending" acceptance is only proven at deploy/QA (no cluster in CI) — the whole-branch review should confirm the CI-testable surface is as strong as it can be and that the deploy-time check is documented; (b) `k8s-openapi` is pinned to `v1_32` — note the resolved kube/k8s-openapi versions in the PR; (c) #283 will consume `CalibanTask` + the agent-sandbox CRD (external schema — a real dependency to resolve before #283).
