# ADR 0004 · `Workspace` CRD + `CalibanTask` resolve-and-pin

- **Status:** accepted
- **Date:** 2026-07-11
- **Source:** Prospero K8s Config Plane design (umbrella design doc:
  `docs/superpowers/plans/2026-07-11-workspace-crd-and-resolve-and-pin.md`) ·
  caliban-operator [#11](https://github.com/caliban-ai/caliban-operator/issues/11) ·
  epic [#274](https://github.com/caliban-ai/caliban/issues/274) · cross-repo:
  prospero mirror (prospero#141), chart RBAC (helm-charts#30) · builds on
  [ADR 0001](0001-kube-rs-stack-and-calibantask-crd.md)

## Context

[ADR 0001](0001-kube-rs-stack-and-calibantask-crd.md) put the workspace (sources,
model/provider config) inline on `CalibanTask.spec.workspace`. Prospero's K8s Config
Plane epic needs a UI where a user edits workspace-level config (git sources, which
model providers are available, which Secret backs each) independently of, and prior
to, launching any task. With the config living only inline on a `CalibanTask`,
prospero has nowhere to persist it: there is no object to CRUD before a task exists,
and no durable home for it once a task completes and its `CalibanTask` is deleted.
Prospero's existing k8s-config-plane endpoint has no backing resource to write to and
returns `405 Method Not Allowed` for exactly this reason.

The epic's shape for this is explicit: prospero becomes a *pure editor of a CRD* — it
CRUDs Kubernetes objects and lets the operator, not prospero, own reconciliation and
credential access. That requires the config to be its own first-class,
independently-lifecycled resource, and it requires prospero to never need Secret read
access (the operator remains the only component that reads Secret values — a
pre-existing, load-bearing security boundary, not a new one introduced here).

Forces:

- **Config must outlive any single task and be editable without one.** Sources and
  provider bindings are workspace-scoped, shared across many `CalibanTask`s, and
  meaningfully exist before the first task is created.
- **Multiple named providers, not one.** A workspace may bind a cheap/local model
  (e.g. Ollama) for routine work and a stronger hosted model (e.g. Anthropic) for
  harder tasks; a task picks one by name at submission time, not at workspace-authoring
  time.
- **Prospero must never read Secrets.** Its CRUD surface is the `Workspace`/
  `CalibanTask` specs only; `credentialsRef` carries a Secret name + key, never a
  value, so the config plane can be edited without granting `secrets` RBAC to
  prospero.
- **Immutable-run guarantee must survive the split.** #283 and #366 (ADRs 0002–0003)
  established that a running task's config is stable for its lifetime. Moving
  provider config out of the task's own spec and into a separately-editable
  `Workspace` must not let a config edit reach into a task that's already running.
- **Pre-`v1alpha1` API, no migration machinery.** As in ADR 0001, `v1alpha1` buys the
  freedom to reshape the CR without a conversion webhook; this ADR spends that budget
  on the workspace split rather than deferring it.

## Decision

1. **A first-class `Workspace` CRD** (`caliban.caliban-ai.dev/v1alpha1`, namespaced,
   status subresource, shortname `cws`) holds the durable, shared config: `sources`
   (the git checkouts), a **named** `providers` list (each `{name, kind, baseUrl,
   model, credentialsRef}` — a `credentialsRef` is a by-name Secret+key reference,
   never a value), an optional `defaultProvider`, non-secret `env`, and default
   `isolation`. `WorkspaceStatus` carries `phase` (`Pending → Reconciling → Ready |
   Failed`), `conditions`, `observedGeneration`, and a human-readable `message`. The
   CRD YAML is generated from the Rust types and golden-tested in sync, per ADR
   0001's discipline.

2. **A pure validation function backs a lightweight status-only reconciler.**
   `validate_workspace(spec, secret_present: Fn(&str, &str) -> bool)` checks unique
   provider names, a resolvable `defaultProvider`, and that every `credentialsRef`
   names an existing Secret key — the *existence* check only. The controller supplies
   `secret_present` by calling the Kubernetes API; **the operator is the sole reader
   of Secret contents** in the system, unchanged from before this ADR but now made
   structural: nothing else needs `secrets` RBAC because nothing else touches
   `Workspace` reconciliation. The controller's only side effect is patching
   `Workspace.status` — it creates no pods, no children; provisioning stays entirely
   in the `CalibanTask` reconcile (ADR 0002).

3. **`CalibanTask` references a `Workspace` by name and resolves-and-pins at
   admission.** `CalibanTaskSpec` gains `workspaceRef: { name }` (required) and
   `providerRef: Option<String>` (which named provider to bind; falls back to the
   workspace's `defaultProvider`, then to the sole provider if there's exactly one).
   `resolve_workspace(spec, provider_ref)` is a pure function that flattens the
   chosen provider and the workspace's sources/env/isolation into a single
   `ResolvedWorkspace`, erroring on a dangling `workspaceRef`/`providerRef` or an
   ambiguous default. The `CalibanTask` reconcile calls it once and pins the result
   into `CalibanTaskStatus.resolvedWorkspace`; if that field is already set, later
   `Workspace` edits are **not** re-resolved into it — the pod is always built from
   the pinned snapshot, preserving the immutable-run guarantee across the split.

4. **Inline `CalibanTask.spec.workspace` is removed, not deprecated.** Per ADR 0001's
   `v1alpha1` bet, this is a breaking, pre-v1 change with no conversion shim: existing
   `CalibanTask` CRs must be re-created against the new schema (`workspaceRef`/
   `providerRef`) with a companion `Workspace` applied first. There is no dual-write
   or fallback path to the old inline shape.

5. **Sample CRs are the cross-repo contract fixture.** `deploy/samples/workspace.yaml`
   and `deploy/samples/calibantask.yaml` are committed, mutually-consistent examples
   (the sample task's `providerRef` names a provider the sample workspace defines) and
   are golden-tested here (`workspace::tests::sample_manifests_deserialize` — both
   deserialize against the Rust types *and* `resolve_workspace` succeeds against
   them). Prospero's mirror types are validated against these same fixtures
   (prospero#141), so this is the shared fixture two repos check against
   independently.

## Consequences

- **Prospero gets a durable object to edit and no path to Secrets.** Workspace
  authoring (sources, provider bindings) no longer requires an existing task, and
  prospero's CRUD surface never needs `secrets` RBAC — `credentialsRef` is
  name-only. This is the change that turns the `405` from the epic's Context into a
  real endpoint.
- **The immutable-run guarantee is preserved, now across two objects instead of
  one.** A running task's pod is built from `status.resolvedWorkspace`, pinned once;
  editing the backing `Workspace` (rotating a provider, changing a source) affects
  only tasks admitted after the edit, never one already running. This mirrors ADR
  0002's status-derivation discipline (compute, compare, patch only on change) applied
  to a resolve instead of a derive.
- **This is a breaking API change with no migration.** Every existing `CalibanTask`
  in any cluster must be replaced (`workspaceRef` + a companion `Workspace`), and
  every caller (prospero, any manual manifests) must move off inline
  `spec.workspace` in lockstep with this operator version. Acceptable pre-1.0 per
  ADR 0001's `v1alpha1` bet; would not be acceptable post-graduation.
- **The operator's own cluster RBAC needs a `Workspace`/`workspaces/status`
  grant.** This is a small addition to the ClusterRole ADR 0002 already required for
  `CalibanTask`; the actual chart change is cross-repo and deferred to
  helm-charts#30, not addressed here.
- **Multi-namespace tenancy remains deferred.** `workspaceRef` is a same-namespace,
  by-name reference (mirroring `WorkspaceRef`'s single `name` field); a
  `CalibanTask` cannot bind to a `Workspace` in another namespace. Cross-namespace
  workspace sharing, if ever needed, is a later ADR.
- **Prospero's mirror types can rot independently of this repo.** The sample CRs
  are the shared contract, but there is no compile-time link between this repo's
  Rust types and prospero's — a schema change here that isn't reflected in
  prospero#141's mirror surfaces only at prospero's own test run, not here.
