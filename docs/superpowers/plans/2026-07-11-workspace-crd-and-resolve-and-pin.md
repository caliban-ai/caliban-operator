# Workspace CRD + resolve-and-pin Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Introduce a first-class `Workspace` CRD with a validation/status reconciler, and switch `CalibanTask` from an inline `spec.workspace` to `workspaceRef`/`providerRef` that the operator resolves and pins at admission — the operator-side foundation (change set B) of the Prospero K8s Config Plane epic (caliban-operator#11).

**Architecture:** A new `Workspace` CRD (`caliban.caliban-ai.dev/v1alpha1`) holds the durable, shared config — `sources[]`, a **named** `providers[]` list (each with its own model + Secret reference), default provider, env, isolation. A lightweight controller validates it (the operator is the *only* component that reads Secrets) and writes `status.phase`. `CalibanTask` now references a `Workspace` by name plus an optional `providerRef`; its controller resolves both at admission, pins the resolved config into `status.resolvedWorkspace` (immutable-run guarantee), and builds the pod from that pinned config. Inline `CalibanTask.spec.workspace` is removed (pre-v1, no back-compat).

**Tech Stack:** Rust 2021 (toolchain 1.95.0), `kube` 4.0 (runtime/derive/client), `k8s-openapi` v1_32, `schemars` 1.2, `serde`/`serde_json`, `serde_norway` (CRD YAML), `tokio`, `thiserror`, `tracing`. Tests are pure-function unit tests + golden CRD serialization (the repo's established `derive_status` pattern) — no live cluster in CI.

## Global Constraints

- **Group/version:** `caliban.caliban-ai.dev/v1alpha1`, namespaced, for both CRDs. Verbatim.
- **CRD field casing:** all spec/status JSON is `camelCase` (`#[serde(rename_all = "camelCase")]` on every struct), matching the existing `CalibanTask`.
- **Secret boundary:** the operator is the **only** component that reads Secret values. Workspace validation checks *existence* of `credentialsRef` (secretName+key); provider credentials reach the pod via `EnvVar.valueFrom.secretKeyRef` (never inlined into the CR or the operator's memory).
- **Pinned-at-admission / immutable run:** once a `CalibanTask` has a non-empty `status.resolvedWorkspace`, later `Workspace` edits must NOT re-pin it. New tasks pick up current config.
- **No back-compat:** inline `CalibanTask.spec.workspace` is removed; existing cluster CRs are re-created under the new schema. No deprecation shim.
- **CRD YAML is generated + golden-tested:** every schema change requires regenerating `deploy/crd/*.yaml` (`cargo run --bin crdgen <kind>`); the `*_is_in_sync` tests guard drift. Never hand-edit the committed YAML.
- **CI gate (mirrors `.github/workflows/ci.yml`):** `cargo fmt --all -- --check` · `cargo clippy --workspace --all-targets -- -D warnings` · `cargo build --workspace --all-targets` · `cargo test --workspace`. All four must pass.

---

## File Structure

- `src/workspace.rs` **(new)** — the `Workspace` CRD (`WorkspaceSpec`, `Provider`, `CredentialsRef`, `EnvEntry`, `WorkspaceStatus`, `WorkspacePhase`, `Workspace` via `CustomResource`), the resolved-config value types (`ResolvedWorkspace`, `ResolvedProvider`), and the two pure functions `validate_workspace()` and `resolve_workspace()`. Uses `crate::crd::{Source, IsolationSpec, Condition}` (same-crate cross-reference — Rust allows it).
- `src/workspace_controller.rs` **(new)** — the `Workspace` controller: fetches referenced Secrets, builds a `secret_present` closure, calls `validate_workspace`, patches `Workspace/status`. Exposes `run(Client) -> anyhow::Result<()>`.
- `src/crd.rs` **(modify)** — `CalibanTaskSpec` loses `workspace`, gains `workspace_ref: WorkspaceRef`, `provider_ref: Option<String>`, `tools: Option<Vec<String>>`; new `WorkspaceRef` struct; `CalibanTaskStatus` gains `resolved_workspace: Option<ResolvedWorkspace>`. The `Workspace` struct (inline aux) is deleted; `Source`/`IsolationSpec`/`Condition` stay here as shared value types.
- `src/resources.rs` **(modify)** — builders consume `&ResolvedWorkspace` (sources + provider env) instead of `t.spec.workspace`; new provider-env projection with `secretKeyRef`. `plan(t, resolved, s)`.
- `src/controller.rs` **(modify)** — CalibanTask reconcile fetches the `Workspace`, resolves + pins into `status.resolvedWorkspace` (once), fails fast on a dangling/invalid ref, feeds the resolved config into `plan`.
- `src/lib.rs` **(modify)** — add `pub mod workspace; pub mod workspace_controller;` and re-exports.
- `src/bin/crdgen.rs` **(modify)** — take a positional `<kind>` arg (`calibantask` | `workspace`) and print that CRD.
- `src/bin/caliban-operator.rs` **(modify)** — run the CalibanTask and Workspace controllers concurrently.
- `deploy/crd/workspace.yaml` **(new, generated)** · `deploy/crd/calibantask.yaml` **(regenerated)**.
- `deploy/samples/workspace.yaml` + `deploy/samples/calibantask.yaml` **(new)** — the cross-repo contract fixtures prospero's mirror validates against.
- `docs/adr/0004-workspace-crd-and-resolve-and-pin.md` **(new)** — records the decision; index row in `docs/adr/README.md`.

---

## Task 1: Workspace CRD types + golden YAML

**Files:**
- Create: `src/workspace.rs`
- Modify: `src/lib.rs`, `src/bin/crdgen.rs`
- Create: `deploy/crd/workspace.yaml`
- Test: inline `#[cfg(test)]` in `src/workspace.rs`

**Interfaces:**
- Produces: `Workspace` (CRD, `caliban.caliban-ai.dev/v1alpha1`), `WorkspaceSpec { display_name: String, sources: Vec<crd::Source>, providers: Vec<Provider>, default_provider: Option<String>, env: Vec<EnvEntry>, isolation: Option<crd::IsolationSpec> }`, `Provider { name: String, kind: String, base_url: Option<String>, model: Option<String>, credentials_ref: Option<CredentialsRef> }`, `CredentialsRef { secret_name: String, key: String }`, `EnvEntry { name: String, value: String }`, `WorkspaceStatus { phase: WorkspacePhase, conditions: Vec<crd::Condition>, observed_generation: Option<i64>, message: Option<String> }`, `WorkspacePhase { Pending, Reconciling, Ready, Failed }` (Default = `Pending`).

- [ ] **Step 1: Add the module declarations**

In `src/lib.rs`, after `pub mod sandbox;` add:

```rust
pub mod workspace;
pub mod workspace_controller;
```

And extend the re-export line:

```rust
pub use crd::{CalibanTask, CalibanTaskSpec, CalibanTaskStatus, Phase};
pub use workspace::{Workspace, WorkspaceSpec, WorkspaceStatus};
```

(`workspace_controller` is created in Task 3; add a temporary empty file now so the crate compiles: `echo '//! placeholder' > src/workspace_controller.rs`.)

- [ ] **Step 2: Write the Workspace CRD types**

Create `src/workspace.rs` with the type definitions (no tests yet):

```rust
//! The `Workspace` custom resource (v1alpha1): durable, shared config — sources,
//! named providers (each with its own model + Secret reference), env, isolation —
//! that `CalibanTask`s reference by name. Owned and reconciled by the operator,
//! which is the sole reader of provider credential Secrets. See ADR 0004 and the
//! Prospero K8s Config Plane design.

use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::crd::{Condition, IsolationSpec, Source};

/// Desired state of a workspace: sources + named providers + defaults.
#[derive(CustomResource, Serialize, Deserialize, Clone, Debug, JsonSchema)]
#[kube(
    group = "caliban.caliban-ai.dev",
    version = "v1alpha1",
    kind = "Workspace",
    namespaced,
    status = "WorkspaceStatus",
    shortname = "cws",
    printcolumn = r#"{"name":"Phase","type":"string","jsonPath":".status.phase"}"#,
    printcolumn = r#"{"name":"Age","type":"date","jsonPath":".metadata.creationTimestamp"}"#
)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceSpec {
    /// Human-friendly dashboard label.
    #[schemars(length(min = 1))]
    pub display_name: String,
    /// The workspace's git checkouts (1..N).
    #[schemars(length(min = 1))]
    pub sources: Vec<Source>,
    /// Named providers (1..N) agents in this workspace can bind to.
    #[schemars(length(min = 1))]
    pub providers: Vec<Provider>,
    /// Provider name agents get when they don't request one. Implicit when
    /// exactly one provider is defined.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_provider: Option<String>,
    /// Non-secret environment injected into every agent pod.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env: Vec<EnvEntry>,
    /// Default isolation for agents launched against this workspace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub isolation: Option<IsolationSpec>,
}

/// A named model provider bound within a workspace.
#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Provider {
    /// Provider identifier, unique within the workspace (e.g. `planner`).
    #[schemars(length(min = 1))]
    pub name: String,
    /// Provider kind (e.g. `ollama`, `anthropic`, `openai`).
    #[schemars(length(min = 1))]
    pub kind: String,
    /// Override base URL (e.g. `http://192.168.1.240:11434`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// Default model for this provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Reference to an existing Secret for this provider's API key. Keyless
    /// providers (e.g. ollama) omit it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credentials_ref: Option<CredentialsRef>,
}

/// A by-name reference to a key within an existing Kubernetes Secret.
#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CredentialsRef {
    /// Name of the Secret (same namespace).
    #[schemars(length(min = 1))]
    pub secret_name: String,
    /// Key within the Secret's data.
    #[schemars(length(min = 1))]
    pub key: String,
}

/// A non-secret environment entry.
#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvEntry {
    /// Variable name.
    #[schemars(length(min = 1))]
    pub name: String,
    /// Variable value.
    pub value: String,
}

/// Observed state of a `Workspace`.
#[derive(Serialize, Deserialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceStatus {
    /// Lifecycle phase.
    #[serde(default)]
    pub phase: WorkspacePhase,
    /// Standard Kubernetes conditions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<Condition>,
    /// The `.metadata.generation` this status reflects.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_generation: Option<i64>,
    /// Human-readable detail (e.g. `provider 'planner': secret 'anthropic-key' not found`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// `Workspace` lifecycle phase.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq, JsonSchema)]
pub enum WorkspacePhase {
    /// Created, not yet reconciled.
    #[default]
    Pending,
    /// Reconcile in progress.
    Reconciling,
    /// Valid — all providers and credential Secrets resolve.
    Ready,
    /// Invalid — see `message`.
    Failed,
}
```

- [ ] **Step 3: Write the golden CRD serialization tests**

Append to `src/workspace.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use kube::CustomResourceExt;

    #[test]
    fn crd_has_correct_group_version_kind() {
        let crd = Workspace::crd();
        assert_eq!(crd.spec.group, "caliban.caliban-ai.dev");
        assert_eq!(crd.spec.names.kind, "Workspace");
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
    fn crd_enforces_non_empty_required_fields() {
        let crd = Workspace::crd();
        let schema = serde_json::to_value(&crd.spec.versions[0].schema).unwrap();
        let spec = &schema["openAPIV3Schema"]["properties"]["spec"]["properties"];
        assert_eq!(spec["sources"]["minItems"], 1);
        assert_eq!(spec["providers"]["minItems"], 1);
        assert_eq!(spec["displayName"]["minLength"], 1);
        let prov = &spec["providers"]["items"]["properties"];
        assert_eq!(prov["name"]["minLength"], 1);
        assert_eq!(prov["kind"]["minLength"], 1);
    }

    #[test]
    fn sample_cr_round_trips() {
        let yaml = r#"
apiVersion: caliban.caliban-ai.dev/v1alpha1
kind: Workspace
metadata: { name: team-a-ws, namespace: team-a }
spec:
  displayName: Team A
  sources:
    - { name: caliban, repo: "git@example:caliban", ref: main, path: /work/caliban }
  providers:
    - { name: planner, kind: anthropic, model: claude-opus-4-8, credentialsRef: { secretName: anthropic-key, key: api-key } }
    - { name: workers, kind: ollama, baseUrl: "http://192.168.1.240:11434", model: qwen2.5-coder }
  defaultProvider: planner
"#;
        let ws: Workspace = serde_norway::from_str(yaml).unwrap();
        assert_eq!(ws.spec.providers.len(), 2);
        assert_eq!(ws.spec.providers[0].name, "planner");
        assert_eq!(
            ws.spec.providers[0]
                .credentials_ref
                .as_ref()
                .unwrap()
                .secret_name,
            "anthropic-key"
        );
        assert!(ws.spec.providers[1].credentials_ref.is_none());
        assert_eq!(ws.spec.default_provider.as_deref(), Some("planner"));
        // camelCase survives round-trip.
        let v = serde_json::to_value(&ws.spec).unwrap();
        assert!(v["displayName"].is_string());
    }

    #[test]
    fn committed_crd_yaml_is_in_sync() {
        let generated = serde_norway::to_string(&Workspace::crd()).unwrap();
        let committed = include_str!("../deploy/crd/workspace.yaml");
        assert_eq!(
            generated.trim(),
            committed.trim(),
            "deploy/crd/workspace.yaml is stale — regenerate: cargo run --bin crdgen workspace > deploy/crd/workspace.yaml"
        );
    }
}
```

- [ ] **Step 4: Run the type/round-trip tests (sync test will fail — expected)**

Run: `cargo test -p caliban-operator workspace::tests::sample_cr_round_trips workspace::tests::crd_has_correct_group_version_kind workspace::tests::crd_enforces_non_empty_required_fields`
Expected: PASS (these three don't need the committed YAML).

- [ ] **Step 5: Teach `crdgen` to emit either CRD, then generate `workspace.yaml`**

Replace `src/bin/crdgen.rs` with:

```rust
//! Emit a CRD's YAML: `cargo run --bin crdgen <calibantask|workspace> > deploy/crd/<kind>.yaml`.

use kube::CustomResourceExt;

fn main() -> anyhow::Result<()> {
    let kind = std::env::args().nth(1).unwrap_or_else(|| "calibantask".into());
    let yaml = match kind.as_str() {
        "calibantask" => serde_norway::to_string(&caliban_operator::crd::CalibanTask::crd())?,
        "workspace" => serde_norway::to_string(&caliban_operator::workspace::Workspace::crd())?,
        other => anyhow::bail!("unknown CRD kind {other:?}; expected calibantask|workspace"),
    };
    print!("{yaml}");
    Ok(())
}
```

Then generate the manifest:

```bash
cargo run --quiet --bin crdgen workspace > deploy/crd/workspace.yaml
```

- [ ] **Step 6: Run the full sync test**

Run: `cargo test -p caliban-operator workspace::tests`
Expected: PASS (all four, including `committed_crd_yaml_is_in_sync`).

- [ ] **Step 7: Commit**

```bash
git add src/lib.rs src/workspace.rs src/workspace_controller.rs src/bin/crdgen.rs deploy/crd/workspace.yaml
git commit -m "feat(crd): add Workspace CRD (v1alpha1) + crdgen kind selector (#11)"
```

---

## Task 2: `validate_workspace()` — pure validation

**Files:**
- Modify: `src/workspace.rs`
- Test: inline `#[cfg(test)]` in `src/workspace.rs`

**Interfaces:**
- Consumes: `WorkspaceSpec`, `Provider`, `CredentialsRef` (Task 1).
- Produces: `WorkspaceValidation { phase: WorkspacePhase, message: Option<String> }` and `pub fn validate_workspace(spec: &WorkspaceSpec, secret_present: impl Fn(&str, &str) -> bool) -> WorkspaceValidation`. `secret_present(secret_name, key)` returns whether that Secret key exists. Cluster access is the caller's job (Task 3) — this function is pure and unit-tested.

- [ ] **Step 1: Write the failing tests**

Append to the `tests` module in `src/workspace.rs`:

```rust
    fn spec_with(providers: Vec<Provider>, default_provider: Option<&str>) -> WorkspaceSpec {
        WorkspaceSpec {
            display_name: "Team A".into(),
            sources: vec![Source {
                name: "caliban".into(),
                repo: "git@x:caliban".into(),
                r#ref: "main".into(),
                path: "/work/caliban".into(),
            }],
            providers,
            default_provider: default_provider.map(String::from),
            env: vec![],
            isolation: None,
        }
    }

    fn provider(name: &str, cred: Option<(&str, &str)>) -> Provider {
        Provider {
            name: name.into(),
            kind: "anthropic".into(),
            base_url: None,
            model: None,
            credentials_ref: cred.map(|(s, k)| CredentialsRef {
                secret_name: s.into(),
                key: k.into(),
            }),
        }
    }

    #[test]
    fn valid_workspace_is_ready() {
        let spec = spec_with(vec![provider("planner", Some(("anthropic-key", "api-key")))], Some("planner"));
        let v = validate_workspace(&spec, |s, k| s == "anthropic-key" && k == "api-key");
        assert_eq!(v.phase, WorkspacePhase::Ready);
        assert!(v.message.is_none());
    }

    #[test]
    fn keyless_provider_needs_no_secret() {
        let mut p = provider("workers", None);
        p.kind = "ollama".into();
        let spec = spec_with(vec![p], None);
        let v = validate_workspace(&spec, |_, _| false); // no secrets exist at all
        assert_eq!(v.phase, WorkspacePhase::Ready);
    }

    #[test]
    fn missing_secret_fails_with_message() {
        let spec = spec_with(vec![provider("planner", Some(("anthropic-key", "api-key")))], None);
        let v = validate_workspace(&spec, |_, _| false);
        assert_eq!(v.phase, WorkspacePhase::Failed);
        assert_eq!(
            v.message.as_deref(),
            Some("provider 'planner': secret 'anthropic-key' key 'api-key' not found")
        );
    }

    #[test]
    fn duplicate_provider_names_fail() {
        let spec = spec_with(vec![provider("planner", None), provider("planner", None)], None);
        let v = validate_workspace(&spec, |_, _| true);
        assert_eq!(v.phase, WorkspacePhase::Failed);
        assert_eq!(v.message.as_deref(), Some("duplicate provider name 'planner'"));
    }

    #[test]
    fn dangling_default_provider_fails() {
        let spec = spec_with(vec![provider("planner", None)], Some("nope"));
        let v = validate_workspace(&spec, |_, _| true);
        assert_eq!(v.phase, WorkspacePhase::Failed);
        assert_eq!(v.message.as_deref(), Some("defaultProvider 'nope' names no provider"));
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p caliban-operator workspace::tests::valid_workspace_is_ready`
Expected: FAIL with "cannot find function `validate_workspace`".

- [ ] **Step 3: Implement `validate_workspace`**

Add to `src/workspace.rs` (above the `tests` module):

```rust
/// Outcome of validating a `Workspace` against known Secret existence.
pub struct WorkspaceValidation {
    /// Derived phase (`Ready` or `Failed`).
    pub phase: WorkspacePhase,
    /// Human-readable failure detail, `None` when `Ready`.
    pub message: Option<String>,
}

/// Pure validation of a `WorkspaceSpec`: unique provider names, a resolvable
/// `defaultProvider`, and an existing Secret key for every `credentialsRef`.
/// `secret_present(secret_name, key)` reports Secret-key existence (cluster
/// lookup is the caller's responsibility). First problem found wins.
pub fn validate_workspace(
    spec: &WorkspaceSpec,
    secret_present: impl Fn(&str, &str) -> bool,
) -> WorkspaceValidation {
    fn failed(message: String) -> WorkspaceValidation {
        WorkspaceValidation {
            phase: WorkspacePhase::Failed,
            message: Some(message),
        }
    }

    let mut seen = std::collections::BTreeSet::new();
    for p in &spec.providers {
        if !seen.insert(p.name.as_str()) {
            return failed(format!("duplicate provider name '{}'", p.name));
        }
    }
    if let Some(dp) = &spec.default_provider {
        if !spec.providers.iter().any(|p| &p.name == dp) {
            return failed(format!("defaultProvider '{dp}' names no provider"));
        }
    }
    for p in &spec.providers {
        if let Some(c) = &p.credentials_ref {
            if !secret_present(&c.secret_name, &c.key) {
                return failed(format!(
                    "provider '{}': secret '{}' key '{}' not found",
                    p.name, c.secret_name, c.key
                ));
            }
        }
    }
    WorkspaceValidation {
        phase: WorkspacePhase::Ready,
        message: None,
    }
}
```

- [ ] **Step 4: Run to verify they pass**

Run: `cargo test -p caliban-operator workspace::tests`
Expected: PASS (all validation tests + the Task 1 tests).

- [ ] **Step 5: Commit**

```bash
git add src/workspace.rs
git commit -m "feat(workspace): pure validate_workspace (provider/secret checks) (#11)"
```

---

## Task 3: Workspace controller + dual-controller run wiring

**Files:**
- Modify: `src/workspace_controller.rs` (replace the placeholder)
- Modify: `src/bin/caliban-operator.rs`
- Test: inline `#[cfg(test)]` in `src/workspace_controller.rs`

**Interfaces:**
- Consumes: `Workspace`, `WorkspaceStatus`, `WorkspacePhase`, `validate_workspace` (Tasks 1–2).
- Produces: `pub async fn run(client: Client) -> anyhow::Result<()>` and `pub(crate) fn derive_workspace_status(ws: &Workspace, v: WorkspaceValidation) -> Option<WorkspaceStatus>` (returns `Some` only when status changed — the no-op-churn pattern from `controller::derive_status`).

- [ ] **Step 1: Write the failing test for `derive_workspace_status`**

Create `src/workspace_controller.rs`:

```rust
//! The `Workspace` controller: validates each `Workspace` (the operator is the
//! sole Secret reader) and writes `status.phase` / `message` /
//! `observedGeneration`. Provisions nothing. See ADR 0004.

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt as _;
use k8s_openapi::api::core::v1::Secret;
use kube::api::{Patch, PatchParams};
use kube::runtime::controller::Action;
use kube::runtime::watcher::Config;
use kube::runtime::Controller;
use kube::{Api, Client, ResourceExt};

use crate::workspace::{
    validate_workspace, Workspace, WorkspacePhase, WorkspaceStatus, WorkspaceValidation,
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crd::Source;
    use crate::workspace::{Provider, WorkspaceSpec};

    fn workspace(gen: i64) -> Workspace {
        let mut ws = Workspace::new(
            "team-a-ws",
            WorkspaceSpec {
                display_name: "Team A".into(),
                sources: vec![Source {
                    name: "caliban".into(),
                    repo: "git@x:caliban".into(),
                    r#ref: "main".into(),
                    path: "/work/caliban".into(),
                }],
                providers: vec![Provider {
                    name: "workers".into(),
                    kind: "ollama".into(),
                    base_url: None,
                    model: None,
                    credentials_ref: None,
                }],
                default_provider: None,
                env: vec![],
                isolation: None,
            },
        );
        ws.metadata.namespace = Some("team-a".into());
        ws.metadata.generation = Some(gen);
        ws
    }

    #[test]
    fn ready_status_is_derived_and_records_generation() {
        let ws = workspace(3);
        let v = validate_workspace(&ws.spec, |_, _| true);
        let s = derive_workspace_status(&ws, v).unwrap();
        assert_eq!(s.phase, WorkspacePhase::Ready);
        assert_eq!(s.observed_generation, Some(3));
        assert!(s.message.is_none());
    }

    #[test]
    fn unchanged_status_is_noop() {
        let mut ws = workspace(3);
        let v = validate_workspace(&ws.spec, |_, _| true);
        ws.status = derive_workspace_status(&ws, v);
        let v2 = validate_workspace(&ws.spec, |_, _| true);
        assert!(derive_workspace_status(&ws, v2).is_none());
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p caliban-operator workspace_controller::tests::ready_status_is_derived_and_records_generation`
Expected: FAIL with "cannot find function `derive_workspace_status`".

- [ ] **Step 3: Implement the controller + `derive_workspace_status`**

Insert above the `tests` module in `src/workspace_controller.rs`:

```rust
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

/// Derive the new `WorkspaceStatus` from a validation result. Returns `Some`
/// only when it differs from the observed status (no-op-churn avoidance,
/// mirroring `controller::derive_status`).
pub(crate) fn derive_workspace_status(
    ws: &Workspace,
    v: WorkspaceValidation,
) -> Option<WorkspaceStatus> {
    let mut next = ws.status.clone().unwrap_or_default();
    next.phase = v.phase;
    next.message = v.message;
    next.observed_generation = ws.metadata.generation;
    match &ws.status {
        Some(cur)
            if cur.phase == next.phase
                && cur.message == next.message
                && cur.observed_generation == next.observed_generation =>
        {
            None
        }
        _ => Some(next),
    }
}

async fn reconcile(ws: Arc<Workspace>, ctx: Arc<Client>) -> Result<Action, Error> {
    let ns = ws.namespace().unwrap_or_default();
    let name = ws.name_any();
    let secrets: Api<Secret> = Api::namespaced((*ctx).clone(), &ns);

    // Resolve which (secretName, key) pairs actually exist, once, up front, so
    // validate_workspace stays pure.
    let mut present: std::collections::BTreeSet<(String, String)> = Default::default();
    for p in &ws.spec.providers {
        if let Some(c) = &p.credentials_ref {
            if let Some(sec) = secrets.get_opt(&c.secret_name).await? {
                let has = sec
                    .data
                    .as_ref()
                    .is_some_and(|d| d.contains_key(&c.key))
                    || sec
                        .string_data
                        .as_ref()
                        .is_some_and(|d| d.contains_key(&c.key));
                if has {
                    present.insert((c.secret_name.clone(), c.key.clone()));
                }
            }
        }
    }
    let validation = validate_workspace(&ws.spec, |s, k| {
        present.contains(&(s.to_string(), k.to_string()))
    });

    if let Some(status) = derive_workspace_status(&ws, validation) {
        let api: Api<Workspace> = Api::namespaced((*ctx).clone(), &ns);
        let phase = status.phase;
        let patch = serde_json::json!({ "status": status });
        api.patch_status(&name, &PatchParams::default(), &Patch::Merge(&patch))
            .await?;
        tracing::info!(%ns, %name, ?phase, "patched Workspace status");
    }
    Ok(Action::requeue(Duration::from_secs(300)))
}

fn error_policy(_ws: Arc<Workspace>, err: &Error, _ctx: Arc<Client>) -> Action {
    tracing::warn!(error = %err, "workspace reconcile error");
    Action::requeue(Duration::from_secs(30))
}

/// Run the Workspace controller until shutdown.
pub async fn run(client: Client) -> anyhow::Result<()> {
    let workspaces: Api<Workspace> = Api::all(client.clone());
    let ctx = Arc::new(client);
    Controller::new(workspaces, Config::default())
        .shutdown_on_signal()
        .run(reconcile, error_policy, ctx)
        .for_each(|res| async move {
            match res {
                Ok((obj, _)) => tracing::debug!(?obj, "workspace reconciled"),
                Err(e) => tracing::warn!(error = %e, "workspace controller error"),
            }
        })
        .await;
    Ok(())
}
```

- [ ] **Step 4: Run to verify the tests pass**

Run: `cargo test -p caliban-operator workspace_controller::tests`
Expected: PASS.

- [ ] **Step 5: Run both controllers concurrently from the entrypoint**

Replace the body of `main` in `src/bin/caliban-operator.rs` (after the client is built) so both controllers run together:

```rust
    let client = kube::Client::try_default().await?;
    tracing::info!("connected to the Kubernetes API");
    tokio::try_join!(
        caliban_operator::controller::run(client.clone()),
        caliban_operator::workspace_controller::run(client),
    )?;
    Ok(())
```

- [ ] **Step 6: Build to confirm the entrypoint compiles**

Run: `cargo build --workspace --all-targets`
Expected: builds clean.

- [ ] **Step 7: Commit**

```bash
git add src/workspace_controller.rs src/bin/caliban-operator.rs
git commit -m "feat(workspace): validation/status controller + run both controllers (#11)"
```

---

## Task 4: Resolved-config types + `resolve_workspace()` — pure resolve

**Files:**
- Modify: `src/workspace.rs`
- Test: inline `#[cfg(test)]` in `src/workspace.rs`

**Interfaces:**
- Consumes: `WorkspaceSpec`, `Provider` (Task 1).
- Produces: `ResolvedProvider { name, kind, base_url: Option<String>, model: Option<String>, credentials_ref: Option<CredentialsRef> }`, `ResolvedWorkspace { sources: Vec<crd::Source>, provider: ResolvedProvider, env: Vec<EnvEntry>, isolation: Option<crd::IsolationSpec> }`, and `pub fn resolve_workspace(spec: &WorkspaceSpec, provider_ref: Option<&str>) -> Result<ResolvedWorkspace, String>`. Both resolved types derive `Serialize, Deserialize, Clone, Debug, JsonSchema` (they are pinned into `CalibanTaskStatus`).

- [ ] **Step 1: Write the failing tests**

Append to the `tests` module in `src/workspace.rs`:

```rust
    #[test]
    fn resolve_picks_named_provider() {
        let spec = spec_with(
            vec![provider("planner", Some(("k", "v"))), provider("workers", None)],
            Some("planner"),
        );
        let r = resolve_workspace(&spec, Some("workers")).unwrap();
        assert_eq!(r.provider.name, "workers");
        assert_eq!(r.sources.len(), 1);
    }

    #[test]
    fn resolve_falls_back_to_default_provider() {
        let spec = spec_with(vec![provider("planner", None), provider("workers", None)], Some("workers"));
        let r = resolve_workspace(&spec, None).unwrap();
        assert_eq!(r.provider.name, "workers");
    }

    #[test]
    fn resolve_uses_sole_provider_when_no_default() {
        let spec = spec_with(vec![provider("only", None)], None);
        let r = resolve_workspace(&spec, None).unwrap();
        assert_eq!(r.provider.name, "only");
    }

    #[test]
    fn resolve_ambiguous_without_default_errors() {
        let spec = spec_with(vec![provider("a", None), provider("b", None)], None);
        let err = resolve_workspace(&spec, None).unwrap_err();
        assert_eq!(err, "no providerRef and workspace has no defaultProvider among 2 providers");
    }

    #[test]
    fn resolve_dangling_provider_ref_errors() {
        let spec = spec_with(vec![provider("planner", None)], None);
        let err = resolve_workspace(&spec, Some("nope")).unwrap_err();
        assert_eq!(err, "providerRef 'nope' names no provider in the workspace");
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p caliban-operator workspace::tests::resolve_picks_named_provider`
Expected: FAIL with "cannot find function `resolve_workspace`".

- [ ] **Step 3: Implement the resolved types + `resolve_workspace`**

Add to `src/workspace.rs` (above the `tests` module):

```rust
/// A provider with its workspace context flattened in — the pinned form.
#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedProvider {
    /// Provider name.
    pub name: String,
    /// Provider kind.
    pub kind: String,
    /// Base URL, if set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// Model, if set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Credential Secret reference, if the provider needs one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credentials_ref: Option<CredentialsRef>,
}

/// The workspace config a `CalibanTask` runs against, resolved to a single
/// provider and pinned into `CalibanTaskStatus.resolvedWorkspace` at admission.
#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedWorkspace {
    /// The workspace's source checkouts.
    pub sources: Vec<Source>,
    /// The single provider this task binds to.
    pub provider: ResolvedProvider,
    /// Non-secret env from the workspace.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env: Vec<EnvEntry>,
    /// Workspace default isolation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub isolation: Option<IsolationSpec>,
}

/// Resolve a `WorkspaceSpec` + optional `providerRef` to a single-provider
/// `ResolvedWorkspace`. Provider selection: explicit `provider_ref` →
/// `defaultProvider` → the sole provider if there's exactly one. Errors on a
/// dangling ref or an ambiguous choice.
pub fn resolve_workspace(
    spec: &WorkspaceSpec,
    provider_ref: Option<&str>,
) -> Result<ResolvedWorkspace, String> {
    let chosen = match provider_ref {
        Some(name) => spec
            .providers
            .iter()
            .find(|p| p.name == name)
            .ok_or_else(|| format!("providerRef '{name}' names no provider in the workspace"))?,
        None => match &spec.default_provider {
            Some(dp) => spec
                .providers
                .iter()
                .find(|p| &p.name == dp)
                .ok_or_else(|| format!("defaultProvider '{dp}' names no provider"))?,
            None if spec.providers.len() == 1 => &spec.providers[0],
            None => {
                return Err(format!(
                    "no providerRef and workspace has no defaultProvider among {} providers",
                    spec.providers.len()
                ))
            }
        },
    };
    Ok(ResolvedWorkspace {
        sources: spec.sources.clone(),
        provider: ResolvedProvider {
            name: chosen.name.clone(),
            kind: chosen.kind.clone(),
            base_url: chosen.base_url.clone(),
            model: chosen.model.clone(),
            credentials_ref: chosen.credentials_ref.clone(),
        },
        env: spec.env.clone(),
        isolation: spec.isolation.clone(),
    })
}
```

- [ ] **Step 4: Run to verify they pass**

Run: `cargo test -p caliban-operator workspace::tests`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/workspace.rs
git commit -m "feat(workspace): ResolvedWorkspace + resolve_workspace provider selection (#11)"
```

---

## Task 5: Provider env projection (with `secretKeyRef`)

**Files:**
- Modify: `src/resources.rs`
- Test: inline `#[cfg(test)]` in `src/resources.rs`

**Interfaces:**
- Consumes: `ResolvedProvider`, `EnvEntry` (Task 4).
- Produces: `pub(crate) fn provider_env(rp: &ResolvedProvider) -> Vec<EnvVar>` — projects a resolved provider to caliband env: `CALIBAN_PROVIDER=<kind>`; `CALIBAN_PROVIDER_BASE_URL=<baseUrl>` if set; `CALIBAN_MODEL=<model>` if set; and, when `credentials_ref` is set, `CALIBAN_API_KEY` via `valueFrom.secretKeyRef` (the operator never inlines the value).

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `src/resources.rs`:

```rust
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
        assert_eq!(get("CALIBAN_PROVIDER").unwrap().value.as_deref(), Some("anthropic"));
        assert_eq!(
            get("CALIBAN_PROVIDER_BASE_URL").unwrap().value.as_deref(),
            Some("https://api.anthropic.com")
        );
        assert_eq!(get("CALIBAN_MODEL").unwrap().value.as_deref(), Some("claude-opus-4-8"));
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
            env.iter().find(|e| e.name == "CALIBAN_PROVIDER").unwrap().value.as_deref(),
            Some("ollama")
        );
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p caliban-operator resources::tests::provider_env_projects_kind_url_model_and_secret_ref`
Expected: FAIL with "cannot find function `provider_env`".

- [ ] **Step 3: Implement `provider_env`**

Add to `src/resources.rs`. Extend the `k8s_openapi` import with `EnvVarSource` and `SecretKeySelector`, then:

```rust
use crate::workspace::ResolvedProvider;

/// Project a resolved provider to caliband container env. Credentials reach the
/// pod via `secretKeyRef` (the operator never inlines the value).
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
```

> Note: `SecretKeySelector.name` is a `String` (not `Option`) in k8s-openapi v1_32. If a compile error says it expects `Option<String>`, wrap with `Some(...)` — but v1_32 uses the plain `String`.

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p caliban-operator resources::tests::provider_env_projects_kind_url_model_and_secret_ref resources::tests::provider_env_keyless_has_no_api_key`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/resources.rs
git commit -m "feat(resources): provider_env projection with secretKeyRef (#11)"
```

---

## Task 6: CalibanTask cutover — refs replace inline workspace; reconciler resolves + pins

This is the breaking cutover. It is one atomic commit because removing `spec.workspace` and switching the builders + reconciler to `ResolvedWorkspace` cannot compile half-done. The new pure logic it depends on (Tasks 4–5) is already tested; here we change the schema, rewire the builders/reconciler, fix all fixture fallout, and regenerate the CalibanTask YAML.

**Files:**
- Modify: `src/crd.rs`, `src/resources.rs`, `src/controller.rs`, `src/config.rs` (test fixtures)
- Modify: `deploy/crd/calibantask.yaml` (regenerated)
- Test: updated inline tests across the above

**Interfaces:**
- Consumes: `ResolvedWorkspace`, `resolve_workspace`, `provider_env` (Tasks 4–5).
- Produces: `WorkspaceRef { name: String }`; `CalibanTaskSpec { workspace_ref: WorkspaceRef, provider_ref: Option<String>, task: TaskSpec, model, state, isolation, resources, lifecycle, tools: Option<Vec<String>> }`; `CalibanTaskStatus.resolved_workspace: Option<ResolvedWorkspace>`; `plan(t: &CalibanTask, rw: &ResolvedWorkspace, s: &Settings) -> ReconcilePlan`; `build_sandbox(t, rw, s)`.

- [ ] **Step 1: Change the CalibanTask CRD types**

In `src/crd.rs`:

1. Delete the `Workspace` struct (the inline `{ sources, services }` type) entirely.
2. Replace the `workspace` field on `CalibanTaskSpec` and add the new fields:

```rust
    /// Reference to the namespace-local `Workspace` this task runs against.
    pub workspace_ref: WorkspaceRef,
    /// Which of the workspace's providers to bind; defaults to the workspace's
    /// `defaultProvider` (or its sole provider).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_ref: Option<String>,
    /// The task itself.
    pub task: TaskSpec,
    // ... existing model / state / isolation / resources / lifecycle unchanged ...
    /// Per-run tool override (allow-list) for this task's agents.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<String>>,
```

3. Add the `WorkspaceRef` struct near `NamedRef`:

```rust
/// A by-name reference to a `Workspace` in the same namespace.
#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceRef {
    /// Workspace object name.
    #[schemars(length(min = 1))]
    pub name: String,
}
```

4. Add the pinned field to `CalibanTaskStatus`:

```rust
    /// Resolved workspace config, pinned at admission (immutable run). Set once;
    /// later `Workspace` edits don't re-pin a running task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_workspace: Option<crate::workspace::ResolvedWorkspace>,
```

5. Update `src/crd.rs`'s own `tests` module: the `SAMPLE`/`minimal_cr_defaults`/`crd_enforces_non_empty_required_fields` fixtures now use `workspaceRef` instead of inline `workspace`. Replace the sample spec bodies, e.g.:

```rust
    const SAMPLE: &str = r#"
apiVersion: caliban.caliban-ai.dev/v1alpha1
kind: CalibanTask
metadata: { name: refactor-auth, namespace: team-a }
spec:
  workspaceRef: { name: team-a-ws }
  providerRef: planner
  task: { prompt: "refactor the auth module", agentType: general-purpose }
  isolation: { runtimeClass: gvisor, worktrees: per-source }
"#;
```

Adjust that test's assertions to the new shape (e.g. `task.spec.workspace_ref.name == "team-a-ws"`, `task.spec.provider_ref.as_deref() == Some("planner")`). In `crd_enforces_non_empty_required_fields`, replace the `workspace.sources`/source-field assertions with `spec["workspaceRef"]["properties"]["name"]["minLength"] == 1` (the `sources` constraint now lives on the Workspace CRD test, Task 1).

- [ ] **Step 2: Rewire the resource builders to consume `ResolvedWorkspace`**

In `src/resources.rs`:

1. `clone_script`, `clone_init_container`, `build_sandbox`, `plan` take `rw: &ResolvedWorkspace` and read `rw.sources` instead of `t.spec.workspace.sources`.
2. `caliband_env(t)` becomes `caliband_env(t, rw)`: keep the existing `state.gonzalo_endpoint` / `model.router_config_ref` env, then extend with `provider_env(&rw.provider)` and the workspace `rw.env` entries:

```rust
fn caliband_env(t: &CalibanTask, rw: &ResolvedWorkspace) -> Vec<EnvVar> {
    let mut e = vec![];
    if let Some(ep) = t.spec.state.as_ref().and_then(|st| st.gonzalo_endpoint.clone()) {
        e.push(env("GONZALO_ENDPOINT", ep));
    }
    if let Some(r) = t.spec.model.as_ref().and_then(|m| m.router_config_ref.clone()) {
        e.push(env("CALIBAN_ROUTER_CONFIG_REF", r));
    }
    e.extend(provider_env(&rw.provider));
    for kv in &rw.env {
        e.push(env(&kv.name, kv.value.clone()));
    }
    e
}
```

3. `build_sandbox` isolation precedence: prefer the task's per-run `t.spec.isolation.runtime_class`, else `rw.isolation.runtime_class`:

```rust
    runtime_class_name: t
        .spec
        .isolation
        .as_ref()
        .and_then(|i| i.runtime_class.clone())
        .or_else(|| rw.isolation.as_ref().and_then(|i| i.runtime_class.clone())),
```

4. `plan(t, rw, s)` threads `rw` into `build_sandbox`.
5. Update the `resources::tests` fixtures: the `task()` helper drops the inline `workspace` and sets `workspace_ref`; add a `resolved()` helper returning a `ResolvedWorkspace` (one source, one `ollama` provider), and pass it to `build_sandbox`/`plan`. Every `build_sandbox(&task(), &s)` becomes `build_sandbox(&task(), &resolved(), &s)`.

```rust
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
                kind: "ollama".into(),
                base_url: None,
                model: None,
                credentials_ref: None,
            },
            env: vec![],
            isolation: None,
        }
    }
```

- [ ] **Step 3: Rewire the CalibanTask reconciler to resolve + pin**

In `src/controller.rs`, `reconcile`:

1. After computing `ns`/`name`, resolve the workspace before planning. Fetch the referenced `Workspace`; if absent or unresolvable, patch the task status to `Failed` with a message and stop (fail-fast). If the task already has a pinned `status.resolved_workspace`, reuse it (immutable run) rather than re-resolving:

```rust
    // Pin once: a running task keeps the config it was admitted with.
    let resolved = match obj.status.as_ref().and_then(|s| s.resolved_workspace.clone()) {
        Some(rw) => rw,
        None => {
            let ws_api: Api<Workspace> = Api::namespaced(ctx.client.clone(), &ns);
            let ws = ws_api.get_opt(&obj.spec.workspace_ref.name).await?;
            match ws.and_then(|w| {
                resolve_workspace(&w.spec, obj.spec.provider_ref.as_deref()).ok()
            }) {
                Some(rw) => {
                    // Persist the pin immediately so it's stable for the run.
                    let patch = serde_json::json!({ "status": { "resolvedWorkspace": rw } });
                    let task_api: Api<CalibanTask> = Api::namespaced(ctx.client.clone(), &ns);
                    task_api
                        .patch_status(&name, &PatchParams::default(), &Patch::Merge(&patch))
                        .await?;
                    rw
                }
                None => {
                    let patch = serde_json::json!({
                        "status": { "phase": Phase::Failed,
                            "conditions": [{ "type": "Ready", "status": "False",
                                "reason": "WorkspaceUnresolved",
                                "message": format!("workspaceRef '{}' / providerRef unresolved", obj.spec.workspace_ref.name) }] }
                    });
                    let task_api: Api<CalibanTask> = Api::namespaced(ctx.client.clone(), &ns);
                    task_api
                        .patch_status(&name, &PatchParams::default(), &Patch::Merge(&patch))
                        .await?;
                    tracing::warn!(%ns, %name, "workspace unresolved; task Failed");
                    return Ok(Action::requeue(Duration::from_secs(30)));
                }
            }
        }
    };
    let p = plan(&obj, &resolved, s);
```

2. Add imports: `use crate::workspace::Workspace; use crate::workspace::resolve_workspace;`.
3. The existing SA/NetworkPolicy/Sandbox apply + `derive_status` block stays, now downstream of `resolved`.
4. Update `controller::tests` fixtures: `task_without_status()` sets `workspace_ref` instead of inline `workspace`; where it calls `derive_status`, no change (that function is unaffected). Any construction of `CalibanTaskSpec` gains `workspace_ref`, `provider_ref: None`, `tools: None` and drops `workspace`.

- [ ] **Step 4: Fix `config.rs` test fixtures**

`src/config.rs`'s `tests::task()` constructs a `CalibanTaskSpec` with inline `workspace`. Replace that literal with the new shape (`workspace_ref: WorkspaceRef { name: "team-a-ws".into() }`, `provider_ref: None`, `tools: None`, no `workspace`). Import `WorkspaceRef`.

- [ ] **Step 5: Build and fix any remaining fallout**

Run: `cargo build --workspace --all-targets`
Expected: eventually clean. Fix every construction of `CalibanTaskSpec` the compiler flags (they all need `workspace_ref`/`provider_ref`/`tools` and must drop `workspace`).

- [ ] **Step 6: Regenerate the CalibanTask CRD YAML**

```bash
cargo run --quiet --bin crdgen calibantask > deploy/crd/calibantask.yaml
```

- [ ] **Step 7: Run the whole test suite**

Run: `cargo test --workspace`
Expected: PASS, including `crd::tests::committed_crd_yaml_is_in_sync` (regenerated) and all rewired fixtures.

- [ ] **Step 8: Commit**

```bash
git add src/crd.rs src/resources.rs src/controller.rs src/config.rs deploy/crd/calibantask.yaml
git commit -m "feat(crd)!: CalibanTask workspaceRef/providerRef resolve-and-pin; remove inline workspace (#11)"
```

---

## Task 7: Samples, ADR, docs, and the full gate

**Files:**
- Create: `deploy/samples/workspace.yaml`, `deploy/samples/calibantask.yaml`
- Create: `docs/adr/0004-workspace-crd-and-resolve-and-pin.md`
- Modify: `docs/adr/README.md`, `README.md`
- Test: a golden test that the sample CRs deserialize against the types

- [ ] **Step 1: Write the sample CRs**

`deploy/samples/workspace.yaml`:

```yaml
apiVersion: caliban.caliban-ai.dev/v1alpha1
kind: Workspace
metadata:
  name: team-a-ws
  namespace: team-a
spec:
  displayName: Team A
  sources:
    - { name: caliban, repo: "git@example:caliban", ref: main, path: /work/caliban }
  providers:
    - name: planner
      kind: anthropic
      model: claude-opus-4-8
      credentialsRef: { secretName: anthropic-key, key: api-key }
    - name: workers
      kind: ollama
      baseUrl: "http://192.168.1.240:11434"
      model: qwen2.5-coder
  defaultProvider: planner
```

`deploy/samples/calibantask.yaml`:

```yaml
apiVersion: caliban.caliban-ai.dev/v1alpha1
kind: CalibanTask
metadata:
  name: refactor-auth
  namespace: team-a
spec:
  workspaceRef: { name: team-a-ws }
  providerRef: workers
  task: { prompt: "refactor the auth module", agentType: general-purpose }
```

- [ ] **Step 2: Write a golden test that the samples deserialize**

Add to `src/workspace.rs` tests:

```rust
    #[test]
    fn sample_manifests_deserialize() {
        let ws: Workspace =
            serde_norway::from_str(include_str!("../deploy/samples/workspace.yaml")).unwrap();
        assert_eq!(ws.spec.providers.len(), 2);
        let ct: crate::crd::CalibanTask =
            serde_norway::from_str(include_str!("../deploy/samples/calibantask.yaml")).unwrap();
        assert_eq!(ct.spec.workspace_ref.name, "team-a-ws");
        assert_eq!(ct.spec.provider_ref.as_deref(), Some("workers"));
        // The sample CalibanTask references a provider the sample Workspace defines.
        let r = resolve_workspace(&ws.spec, ct.spec.provider_ref.as_deref()).unwrap();
        assert_eq!(r.provider.kind, "ollama");
    }
```

Run: `cargo test -p caliban-operator workspace::tests::sample_manifests_deserialize`
Expected: PASS.

- [ ] **Step 3: Write ADR 0004**

Create `docs/adr/0004-workspace-crd-and-resolve-and-pin.md` following the format of `0002-reconcile-calibantask-to-sandbox.md`: **Context** (inline `spec.workspace` has nowhere to persist provider/source config → prospero's k8s config plane returns 405; the epic makes prospero a pure editor of a CRD), **Decision** (first-class `Workspace` CRD with named providers; operator is sole Secret reader; `CalibanTask` gains `workspaceRef`/`providerRef` resolved and pinned at admission; inline `spec.workspace` removed, pre-v1 breaking), **Consequences** (immutable-run preserved via pinning; prospero gets no `secrets` RBAC; multi-namespace tenancy still deferred). Reference the umbrella design doc.

- [ ] **Step 4: Add the ADR index row**

In `docs/adr/README.md`, add a row for `0004` matching the existing table format, status **Accepted**.

- [ ] **Step 5: Update the README dependency/architecture note**

If `README.md` documents the CRDs or the reconcile model, add the `Workspace` CRD and the `workspaceRef` resolve-and-pin flow. Keep it to the existing section's style.

- [ ] **Step 6: Run the full CI gate**

```bash
cargo fmt --all
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo build --workspace --all-targets
cargo test --workspace
```

Expected: all four green.

- [ ] **Step 7: Commit**

```bash
git add deploy/samples docs/adr src/workspace.rs README.md
git commit -m "docs(crd): Workspace/CalibanTask sample CRs + ADR 0004 (#11)"
```

---

## Self-Review Notes

- **Spec coverage:** Workspace CRD + generated manifest (T1), validation-and-status reconciler / operator-sole-Secret-reader (T2–T3), `workspaceRef`/`providerRef` + resolve-and-pin + `status.resolvedWorkspace` (T4, T6), inline-workspace removal / breaking (T6), RBAC — **cross-repo (helm-charts#30), out of scope here; called out in ADR 0004**, sample CRs for the cross-repo contract test (T7). Golden CRD serialization (T1, T6), reconciler pure-fn unit tests (T2, T3, T4, T5), resolve+pin (T4, T6). Missing from *this* plan by design: prospero mirror tests (prospero#141), envtest (repo has no harness; substituted by pure-fn + golden per the design's own CI caveat).
- **Type consistency:** `ResolvedWorkspace`/`ResolvedProvider`/`CredentialsRef`/`EnvEntry` names are used identically in `workspace.rs` (defined), `resources.rs` (`provider_env`), `crd.rs` (`CalibanTaskStatus.resolved_workspace`), and `controller.rs` (pin). `resolve_workspace(spec, Option<&str>) -> Result<ResolvedWorkspace, String>` and `validate_workspace(spec, impl Fn(&str,&str)->bool) -> WorkspaceValidation` signatures are stable across their call sites.
- **Open verification risk:** `SecretKeySelector.name` type (plain `String` in k8s-openapi v1_32) — noted inline in T5; confirm at build. The k8s paths (secret read, status patch) are compile- and unit-verified only; a live-cluster smoke is the real gate (design reality-caveat).
