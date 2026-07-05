# ADR 0002 · Reconcile `CalibanTask` → agent-sandbox `Sandbox` (+ per-task SA & NetworkPolicy)

- **Status:** accepted
- **Date:** 2026-07-04
- **Source:** k8s system-design spec (§"`caliban-operator`", §"agent-sandbox integration") in the caliban-ai docs hub · caliban [#283](https://github.com/caliban-ai/caliban/issues/283) · epic [#274](https://github.com/caliban-ai/caliban/issues/274) · builds on [ADR 0001](0001-kube-rs-stack-and-calibantask-crd.md)

## Context

[ADR 0001](0001-kube-rs-stack-and-calibantask-crd.md) fixed the controller stack
(kube 4.0 / k8s-openapi 0.28 / `v1_32`) and the `CalibanTask` API. #282 shipped a
placeholder reconcile that only initialized `.status.phase = Pending`. #283 makes
the reconcile real: turn a `CalibanTask` into a running, sandboxed caliband pod
reachable at a stable DNS, with per-task RBAC and network isolation, and drive
`.status` from the backing Sandbox.

The spec is explicit that the operator is a *thin caliban-semantics layer over
agent-sandbox* — it must not reimplement pods/isolation. So the reconcile composes
existing objects rather than managing pods directly.

Forces:

- **agent-sandbox owns the `Sandbox` CRD** (`agents.x-k8s.io/v1beta1`). We consume
  it — we must not generate, install, or drift-guard its schema. Verified v0.5.0
  `SandboxSpec` = `{ podTemplate{metadata,spec: core.PodSpec} (required), service
  bool, operatingMode enum(Running|Suspended)=Running, shutdownPolicy
  enum(Delete|Retain)=Retain, shutdownTime, volumeClaimTemplates []core.PVC }`;
  `SandboxStatus.serviceFQDN` is the pod's stable DNS.
- **Reconcile must be idempotent and self-healing** — re-running on the same
  `CalibanTask` must converge, not duplicate, and children must be garbage-collected
  when the `CalibanTask` is deleted.
- **Cluster-agnostic** (the eventual charts are public): no home-cluster
  identifiers baked into generated objects.
- **k3s v1.31 target vs `v1_32` compile surface** (ADR 0001): stick to long-stable
  `PodSpec`/`NetworkPolicy` fields.
- The caliband pod needs **zero** Kubernetes API access; its identity exists only
  to be a NetworkPolicy/PodSecurity subject.

## Decision

1. **Consume `Sandbox` as a foreign typed resource.** Hand-author a minimal Rust
   `Sandbox` via `#[derive(CustomResource)]` pointed at
   `group = "agents.x-k8s.io", version = "v1beta1", kind = "Sandbox", namespaced,
   status = "SandboxStatus"`, declaring only the fields we set or read
   (`podTemplate`, `service`, `operatingMode`, `shutdownPolicy`, `shutdownTime`,
   `volumeClaimTemplates`; status `serviceFQDN`, `conditions`). `podTemplate`
   reuses `k8s_openapi` `PodTemplateSpec`; `volumeClaimTemplates` reuses
   `PersistentVolumeClaim`. We **never** call `Sandbox::crd()`, emit its YAML, or
   add it to the drift-sync test — agent-sandbox installs its own CRD. Extra
   `SandboxSpec` fields we omit are pruned by the API server's structural schema;
   this is a read/write view, not the schema of record.

2. **Reconcile = pure builders + a thin apply loop.** The mapping
   `CalibanTask → {Sandbox, ServiceAccount, NetworkPolicy}` and the status
   derivation are **pure functions** (`build_sandbox`, `build_service_account`,
   `build_network_policy`, `derive_status`), unit-tested with no cluster. The async
   reconcile only: server-side-applies each child, reads back the Sandbox, and
   patches `CalibanTask.status`.

3. **Idempotency via server-side apply + owner references.** Children are applied
   with `Patch::Apply` under field manager `caliban-operator` (force), so
   re-reconciles converge on one object. Every child carries a controller
   `OwnerReference` to its `CalibanTask`, so deleting the task cascades. Child names
   are deterministic: `<task>-sbx` (Sandbox), `<task>-sa` (ServiceAccount),
   `<task>-netpol` (NetworkPolicy).

4. **Least-privilege RBAC = a dedicated, token-less ServiceAccount with no bound
   Role.** The caliband pod is given its own per-task `ServiceAccount` with
   `automountServiceAccountToken: false`, referenced from the podTemplate. Because
   no `Role`/`RoleBinding` is created, the pod holds **zero** API permissions —
   strictly the least privilege. An empty Role would be noise; the token-less SA
   *is* the RBAC posture. (The operator's *own* cluster RBAC — the right to create
   Sandboxes/SAs/NetworkPolicies — is deploy-time chart concern #284, not this
   reconcile.)

5. **NetworkPolicy: default-deny with the minimal working allowances.** The
   per-task `NetworkPolicy` selects the Sandbox's pod labels and sets
   `policyTypes: [Ingress, Egress]` with: deny-all by default, **allow DNS egress**
   (UDP/TCP 53), **allow general egress** (git clone + provider APIs; MVP allows
   egress to all non-cluster destinations), and **allow ingress on the caliband
   port** (8443) from the task's namespace. The spec's finer rules
   (prospero→caliband, pod→gonzalo) require cross-namespace identity the CR does
   not carry generically at MVP; they are parameterized in a later iteration. The
   MVP ships a real, cluster-agnostic default-deny posture (selectors are the
   pod's own labels — no home-cluster identifiers).

6. **Status is derived from the Sandbox, not initialized-and-frozen.** This
   evolves #282's init-once no-op: `derive_status(task, sandbox)` returns
   `Provisioning` once the Sandbox is applied but has no `serviceFQDN` yet,
   `Running` once `serviceFQDN` is populated, and sets
   `calibandEndpoint = "<serviceFQDN>:8443"` and `sandboxRef` accordingly. `Pending`
   remains the pre-provision default (a `CalibanTask` observed before its first
   successful apply). The status patch is skipped when the derived status equals
   the observed one (no-op churn avoidance, same discipline as #282).

7. **Explicit deferrals (out of #283 scope, tracked upstream):** the caliban-aware
   drain finalizer (needs caliband's checkpoint gRPC — deferred with the transport
   #314), idle→pause/resume mapping, `SandboxTemplate`/`SandboxWarmPool` (Phase 4),
   and full `Secret`/`ConfigMap` credential projection. #283 projects only what the
   acceptance path needs: the workspace PVC, the caliband image + runtimeClass, and
   the gonzalo endpoint / model-router refs as env, so the pod comes up reachable.

## Consequences

- **A `CalibanTask` now materializes real cluster objects.** Applying one yields a
  Sandbox (→ caliband pod on the configured RuntimeClass, workspace PVC, stable
  DNS), a token-less SA, and a default-deny NetworkPolicy; `.status` reports
  `Provisioning → Running` with `calibandEndpoint`. This satisfies #283's
  acceptance and unblocks prospero's `K8sFleet` (#64) reading the endpoint.
- **agent-sandbox must be installed in the cluster** for the applies to succeed
  (its `Sandbox` CRD must exist). This is the cluster prerequisite the umbrella
  chart bundles by default (helm-charts#6); without it the operator's Sandbox apply
  fails and the task stays `Provisioning` — an honest, retried error, not a crash.
- **The operator needs cluster RBAC to create these children** — it can create
  `sandboxes.agents.x-k8s.io`, `serviceaccounts`, and
  `networkpolicies.networking.k8s.io`, and patch `calibantasks/status`. That
  ClusterRole ships with the operator chart (#284); this ADR fixes the exact verb
  set that chart must grant.
- **Deferring the drain finalizer** means a deleted `CalibanTask` tears down its
  Sandbox immediately (owner-ref GC) without checkpointing in-flight agents. This
  is acceptable for the MVP and revisited when caliband checkpoint gRPC lands.
- **The foreign `Sandbox` view can rot** if agent-sandbox changes its v1beta1
  schema. We pin the consumed version (`v1beta1`) and cover the fields we use with
  round-trip tests against the published CRD's shape; a breaking upstream change
  surfaces as a deserialize/apply failure, caught in integration.
