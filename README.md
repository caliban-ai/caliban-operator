# caliban-operator

[![ci](https://github.com/caliban-ai/caliban-operator/actions/workflows/ci.yml/badge.svg)](https://github.com/caliban-ai/caliban-operator/actions/workflows/ci.yml)
[![license: AGPL-3.0](https://img.shields.io/badge/license-AGPL--3.0-blue.svg)](LICENSE)

Kubernetes operator (Rust / [kube-rs](https://kube.rs)) for **caliban** agent
workloads. It composes the Kubernetes SIG
[agent-sandbox](https://agent-sandbox.sigs.k8s.io) project and reconciles a
`Workspace` + `CalibanTask` pair of custom resources into a sandboxed agent pod.

> **Status:** both CRDs and both reconcile loops are implemented and running in a
> homelab cluster. The API is `v1alpha1` and may still change without conversion
> webhooks. See [`docs/adr/`](docs/adr/README.md) for the accepted decisions, the
> cross-repo k8s system-design spec in the caliban-ai docs hub, and the umbrella
> epic [caliban-ai/caliban#274](https://github.com/caliban-ai/caliban/issues/274).
>
> Leader election is on by default, so more than one replica is safe (#48).
>
> Not yet implemented: the drain/checkpoint lifecycle (so `Draining` is never
> reached and a deleted task is torn down without checkpointing — #42, waiting on
> prospero reporting `AgentsDrained`), and warm pools / `SandboxTemplate`
> selection.

- **[CRD reference](docs/crds.md)** — every `Workspace` and `CalibanTask` field,
  its default, phases, conditions and Events.
- **[Deploying and configuring](docs/deploying.md)** — prerequisites, every
  environment variable, the RBAC the operator needs, and troubleshooting.
- **[Container image](docs/container.md)** — what ships and how it is built.
- **[Samples](deploy/samples/)** and **[generated CRDs](deploy/crd/)**.

## Role in the system

```
prospero --CRUD--------> Workspace CR (sources, named providers, credentialsRef,
                          egress, agentPolicy)
                          (this operator validates + reports status)

prospero --CRUD/watch--> CalibanTask CR (workspaceRef, providerRef, posture)
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
  [ADR 0004](docs/adr/0004-workspace-crd-and-resolve-and-pin.md).
- **Provisioning plane:** the operator owns Sandbox/pod lifecycle, RBAC, and
  NetworkPolicy; prospero needs only CRUD on `Workspace`/`CalibanTask`. Each task
  gets a token-less ServiceAccount with no bound Role — zero API permissions — and
  a `policyTypes: [Ingress, Egress]` NetworkPolicy.
- **Session plane:** live agent streaming/steering goes directly to caliband
  over gRPC/TLS (not through this operator).

### Who owns what

The operator is deliberately **not** a caliband client — it has no TLS stack and
no protocol-version dependency on caliban's wire format
([ADR 0005](docs/adr/0005-operator-infrastructure-prospero-agent-lifecycle.md)).

| Concern | Owner |
|---|---|
| Sandbox, pod, ServiceAccount, NetworkPolicy, workspace PVC | **operator** |
| `status.phase`, `calibandEndpoint`, `sandboxRef`, `resolvedWorkspace`, `permissionPosture`, the `Ready` condition | **operator** |
| Spawning, attaching, streaming and polling agents | **prospero** |
| The `AgentsSettled` condition, which drives the terminal phases | **prospero** |
| caliband's flag/env names and the control wire types (`caliban-contract`) | **caliban** |

They meet in `CalibanTask.status` through **server-side apply field ownership**:
`conditions` is a map-list keyed by `type`, each component applies only its own
entries under its own field manager, and neither clobbers the other. One
consequence worth knowing: without prospero deployed, a finished task stays
`Running`, because only caliband's agent list can tell "agent running" from
"agent finished, pod still up".

## Security posture

- **Credentials never pass through a client.** A `Workspace` names a Secret and
  key; the operator reads only whether that key exists, and projects it into the
  pod as a `secretKeyRef`. It never inlines a value, and it holds `get` on
  Secrets but not `list` or `watch`.
- **Agent pods hold zero Kubernetes API permissions** — a per-task, token-less
  ServiceAccount with no `Role`.
- **Egress is restrictable per workspace.** `Workspace.spec.egress.allow` replaces
  the default allow-all egress with DNS plus exactly the listed CIDRs/ports. It is
  a Workspace-level field precisely so a task's own `spec.isolation` override
  cannot widen it.
- **Unsupervised agents need a Workspace to authorize them — through one
  switch.** `agentPolicy.allowUnattended: true` gates *both* a task asking for
  `permissionPosture: unattended` *and* the Workspace's own unsupervised
  settings (`permissionMode: dontAsk` or `bypassPermissions`, `autoAllow: true`,
  `noPermissions: true`). Either way the check runs before pinning and fails
  closed with reason `PostureNotPermitted`, naming what was set, so the same
  power cannot have one gated door and one ungated one
  ([ADR 0006](docs/adr/0006-unattended-permission-posture-authorized-by-workspace-policy.md),
  decision 7). The gate is only as strong as Workspace RBAC: **treat write
  access to `workspaces` as privileged.**
- **Sandbox pods run non-root**, every container as one uid/gid with `fsGroup`
  set to match, so the git-clone step and the agent cannot drift apart.

## Development

Requires the pinned toolchain in `rust-toolchain.toml` (Rust 1.95.0). The local
gate mirrors `.github/workflows/ci.yml` exactly — run all four:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo build --workspace --all-targets
cargo test --workspace
```

The CRD YAML in `deploy/crd/` is **generated** from the Rust types and committed;
a test fails if it drifts. After any change to `src/crd.rs` or `src/workspace.rs`:

```sh
cargo run --bin crdgen calibantask > deploy/crd/calibantask.yaml
cargo run --bin crdgen workspace   > deploy/crd/workspace.yaml
```

Never hand-edit the committed YAML. The `Sandbox` type in `src/sandbox.rs` is a
read/write view of a CRD agent-sandbox owns — it is never emitted or
drift-guarded.

Architecture decisions go in [`docs/adr/`](docs/adr/README.md), MADR-lite, one
append-only file per decision, starting from
[`docs/adr/template.md`](docs/adr/template.md). A decision is changed by writing
a new ADR that supersedes the old one, never by rewriting history.

## License

AGPL-3.0-only.
