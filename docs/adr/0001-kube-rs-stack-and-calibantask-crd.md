# ADR 0001 · kube-rs stack + `CalibanTask` CRD API

- **Status:** accepted
- **Date:** 2026-07-04
- **Source:** k8s system-design spec (§"`CalibanTask` CR + workspace model", §"`caliban-operator`") in the caliban-ai docs hub · caliban [#282](https://github.com/caliban-ai/caliban/issues/282) · epic [#274](https://github.com/caliban-ai/caliban/issues/274)

## Context

The k8s epic (#274) needs a Kubernetes operator that reconciles a `CalibanTask`
custom resource into a sandboxed caliband pod (via agent-sandbox). The repository
was stood up in #277 as bare scaffolding (an empty `caliban-operator` crate, AGPL,
Rust 1.95, a fmt/clippy/build/test CI). This ADR records the foundational,
hard-to-change decisions that #282 commits to: the controller stack and the
`CalibanTask` API surface (group/version/kind), which becomes a compatibility
contract the moment a CR is applied to a cluster.

Constraints and forces:

- The operator is a *thin caliban-semantics layer over agent-sandbox* — it must not
  reimplement pods/isolation/warm-pools. So the stack is a controller runtime, not
  a pod scheduler.
- The target homelab runs **k3s v1.31**; the CRD and the client must work there,
  and the `k8s-openapi` version pin selects the Kubernetes API surface the operator
  compiles against.
- kube-rs pairs `kube` with a specific `k8s-openapi` major; the two must be chosen
  together or the build won't resolve. Picking versions by hand is brittle.
- The spec fixes the CR shape (`workspace.sources`, `task`, `model`, `state`,
  `isolation`, `resources`, `lifecycle`; status `phase`/`calibandEndpoint`/
  `sandboxRef`/`checkpointRef`/`workspace.materialized`/`conditions`).

## Decision

1. **Controller stack: kube-rs.** Depend on `kube` (features `runtime`, `derive`,
   `client`) + `k8s-openapi` (feature `v1_32`) + `schemars` (CRD schema
   generation) + `tokio` + `serde`/`serde_json` + `serde_yaml` (CRD YAML
   emission) + `thiserror` + `tracing` + `futures`. Exact compatible versions are
   resolved with `cargo add` (which honors kube's `k8s-openapi` pairing) and then
   pinned in `Cargo.lock`; we do not hand-pick the kube/k8s-openapi pair. The
   resolved pair is **kube 4.0 + k8s-openapi 0.28**, whose lowest offered API
   feature is `v1_32` (there is no `v1_31` for this pairing). `v1_32` is the
   compiled API surface; it is forward-compatible with the target **k3s v1.31**
   apiserver for the objects we use (CRD `apiextensions.k8s.io/v1`, core objects,
   and the agent-sandbox CRDs consumed in #283) — the Kubernetes API is tolerant
   across a one-minor skew for these.

2. **API: `caliban.caliban-ai.dev/v1alpha1`, kind `CalibanTask`, namespaced, with a
   status subresource.** Defined via `#[derive(CustomResource, …)]` on a
   `CalibanTaskSpec` struct mirroring the spec's fields, with a separate
   `CalibanTaskStatus`. `v1alpha1` signals the API is unstable and may change
   without conversion webhooks — appropriate for an MVP; graduation to `v1beta1`/`v1`
   is a later ADR. The status subresource keeps spec (intent) and status (observed)
   on separate update paths, as Kubernetes convention requires for controllers.

3. **CRD YAML is generated, not hand-written.** A `crdgen` binary emits
   `CalibanTask::crd()` as YAML; the generated CRD is committed (so the helm chart
   in #284 and `kubectl apply` consumers have a stable artifact) and a test asserts
   the committed YAML is in sync with the Rust types (regenerate-and-diff), so the
   struct is the single source of truth.

4. **#282 delivers a no-op reconcile that sets `.status.phase = Pending`.** The
   controller skeleton watches `CalibanTask`, and its reconcile only initializes
   status to `Pending` and requeues — it creates no Kubernetes objects. The real
   reconcile (`CalibanTask` → agent-sandbox `Sandbox` + RBAC/NetworkPolicy) is
   #283. The `phase` enum is the spec's lifecycle:
   `Pending → Provisioning → Running → Draining → Completed | Failed`.

## Consequences

- **Positive:** the operator has a real, cluster-installable CRD and a running (if
  inert) controller — the #282 acceptance — on a conventional, well-supported
  Rust k8s stack. The generated-and-checked CRD makes the Rust types authoritative
  and gives #284's chart a stable file to package. `v1alpha1` buys freedom to
  iterate the API through P2/P3 without conversion machinery.
- **Negative:** `v1alpha1` means the CR shape can break between releases with no
  automated migration — acceptable pre-1.0, but every CR author is exposed to it.
  Pinning `k8s-openapi` to `v1_32` ties the compiled API surface to that release;
  bumping it later is a deliberate, tested change. Committing generated YAML adds a
  keep-in-sync test that must be run when the types change (the test is the guard).
- **Revisit if:** the API stabilizes enough to warrant `v1beta1`/`v1` + a conversion
  webhook; the target cluster's Kubernetes version moves far enough that the
  `v1_32` `k8s-openapi` surface needs raising (or drops below the tolerated skew
  with k3s v1.31); or agent-sandbox's own API (consumed from #283) forces a
  different `k8s-openapi` pairing.
