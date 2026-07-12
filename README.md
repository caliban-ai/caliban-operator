# caliban-operator

[![ci](https://github.com/caliban-ai/caliban-operator/actions/workflows/ci.yml/badge.svg)](https://github.com/caliban-ai/caliban-operator/actions/workflows/ci.yml)
[![license: AGPL-3.0](https://img.shields.io/badge/license-AGPL--3.0-blue.svg)](LICENSE)

Kubernetes operator (Rust / [kube-rs](https://kube.rs)) for **caliban** agent
workloads. It composes the Kubernetes SIG
[agent-sandbox](https://agent-sandbox.sigs.k8s.io) project and reconciles a
`Workspace` + `CalibanTask` pair of custom resources into a sandboxed agent pod.

> **Status:** the `Workspace` and `CalibanTask` CRDs and their reconcile loops are
> implemented (see [`docs/adr/`](docs/adr/README.md) for the accepted decisions).
> See the full design in the [k8s system design spec](https://github.com/caliban-ai/caliban)
> and the umbrella epic **caliban-ai/caliban#274**.

## Role in the system

```
prospero --CRUD--------> Workspace CR (sources, named providers, credentialsRef)
                          (this operator validates + reports status)

prospero --CRUD/watch--> CalibanTask CR (workspaceRef, providerRef)
                          --reconcile--> agent-sandbox Sandbox pod
                          (this operator)    caliband + caliban agents
```

- **Config plane:** a namespaced `Workspace` CR holds durable, shared config —
  git `sources` and named model `providers` (each with an optional
  `credentialsRef` naming a Secret+key). The operator is the **sole** reader of
  Secret values; prospero only ever sees the by-name reference. A `CalibanTask`
  points at a `Workspace` via `workspaceRef` plus an optional `providerRef`; the
  operator resolves the referenced provider and pins it into
  `status.resolvedWorkspace` at admission, so a running task's config can't shift
  underneath it even if the `Workspace` is edited later. See
  [ADR 0004](docs/adr/0004-workspace-crd-and-resolve-and-pin.md). Sample CRs live in
  [`deploy/samples/`](deploy/samples/).
- **Provisioning plane:** the operator owns Sandbox/pod lifecycle, RBAC, and
  NetworkPolicy; prospero needs only CRUD on `Workspace`/`CalibanTask`.
- **Session plane:** live agent streaming/steering goes directly to caliband
  over gRPC/TLS (not through this operator).

## License

AGPL-3.0-only.
