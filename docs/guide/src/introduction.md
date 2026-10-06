# caliban-operator

caliban-operator is the **Kubernetes operator for [caliban](https://github.com/caliban-ai/caliban)
agent workloads**. It reconciles two custom resources — a `Workspace` and a
`CalibanTask` — into a sandboxed pod running `caliband` and its agents, composing
the Kubernetes SIG [agent-sandbox](https://agent-sandbox.sigs.k8s.io) project
rather than managing pods itself.

It is written in Rust on [kube-rs](https://kube.rs).

```text
Status: both CRDs and both reconcile loops are implemented and running in a
homelab cluster. The API is v1alpha1 and may still change without conversion
webhooks.
```

## Where it sits

The operator owns *infrastructure*; [prospero](https://github.com/caliban-ai/prospero)
owns *agent lifecycle*. That split is deliberate and recorded in
[ADR 0005](./adr/0005-operator-infrastructure-prospero-agent-lifecycle.md).

```text
prospero ──CRUD────────▶ Workspace CR        sources, named providers,
                                             credentialsRef, egress, agentPolicy
                                             (this operator validates + reports status)

prospero ──CRUD/watch──▶ CalibanTask CR  ──reconcile──▶  agent-sandbox Sandbox pod
                         workspaceRef,                   caliband + caliban agents
                         providerRef, posture
```

Prospero creates and watches the custom resources; it never creates a pod. The
operator never decides which agent runs or when — it decides whether a task is
*admissible*, pins the configuration it will run under, and reconciles the
sandbox that results.

## The two resources

- **`Workspace`** is the configuration plane: git sources, named model providers
  and their credentials, egress rules, storage settings, and the agent policy that
  governs what an agent in this workspace is allowed to do.

- **`CalibanTask`** is one unit of work. It references a `Workspace` and
  (optionally) one of its named providers, and carries the task itself.

When a task is admitted, the operator **resolves and pins** the workspace
configuration into `status.resolvedWorkspace`, once. Everything the sandbox is
built from comes from that pinned snapshot, so editing the `Workspace` afterwards
cannot change a running task underneath itself. That rule is
[ADR 0004](./adr/0004-workspace-crd-and-resolve-and-pin.md).

## Permission posture

A task declares a `permissionPosture`. `unattended` — an agent whose tool calls
are never held for a human — is admitted **only** under a `Workspace` whose
`agentPolicy.allowUnattended` is true. The workspace, not the task, is where that
authority is granted, so a task cannot promote itself. The same rule governs the
workspace's own unsupervised `agentPolicy` settings. See
[ADR 0006](./adr/0006-unattended-permission-posture-authorized-by-workspace-policy.md).

The effective posture is reported in `status` and shown as a `Posture` printer
column, so `kubectl get ctask` says what a task actually got rather than what it
asked for.

## Where to go next

- **[CRD reference](./crds.md)** — every `Workspace` and `CalibanTask` field, its
  default, plus phases, conditions and the Events the operator emits.
- **[Deploying and configuring](./deploying.md)** — prerequisites, every
  environment variable, the RBAC the operator needs, and troubleshooting.
- **[Container image](./container.md)** — what ships and how it is built.
- **[Changelog](./changelog.md)** — what changed in each release.
- **[Architecture Decision Records](./adr/index.md)** — the accepted decisions and
  why they were made.
