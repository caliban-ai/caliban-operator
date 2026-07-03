# caliban-operator

[![ci](https://github.com/caliban-ai/caliban-operator/actions/workflows/ci.yml/badge.svg)](https://github.com/caliban-ai/caliban-operator/actions/workflows/ci.yml)
[![license: AGPL-3.0](https://img.shields.io/badge/license-AGPL--3.0-blue.svg)](LICENSE)

Kubernetes operator (Rust / [kube-rs](https://kube.rs)) for **caliban** agent
workloads. It composes the Kubernetes SIG
[agent-sandbox](https://agent-sandbox.sigs.k8s.io) project and reconciles a
`CalibanTask` custom resource into a sandboxed agent pod.

> **Status:** repository scaffolding only. The `CalibanTask` CRD and the
> reconcile loop land in follow-up tickets. See the full design in the
> [k8s system design spec](https://github.com/caliban-ai/caliban) and the
> umbrella epic **caliban-ai/caliban#274**.

## Role in the system

```
prospero --CRUD/watch--> CalibanTask CR --reconcile--> agent-sandbox Sandbox pod
                          (this operator)                caliband + caliban agents
```

- **Provisioning plane:** the operator owns Sandbox/pod lifecycle, RBAC, and
  NetworkPolicy; prospero needs only CRUD on `CalibanTask`.
- **Session plane:** live agent streaming/steering goes directly to caliband
  over gRPC/TLS (not through this operator).

## License

AGPL-3.0-only.
