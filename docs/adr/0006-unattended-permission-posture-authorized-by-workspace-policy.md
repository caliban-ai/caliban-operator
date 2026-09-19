# ADR 0006 · Unattended permission posture is authorized by Workspace policy

- **Status:** accepted
- **Date:** 2026-09-18
- **Source:** caliban-operator [#80](https://github.com/caliban-ai/caliban-operator/issues/80) ·
  caliban ADR 0059 (per-session permission posture), caliban
  [#676](https://github.com/caliban-ai/caliban/issues/676) (`SpawnSpec.permission_posture`) ·
  prospero ADR 0010 (inbound API scopes) · builds on [ADR 0004](0004-workspace-crd-and-resolve-and-pin.md)
  (resolve-and-pin) and [ADR 0005](0005-operator-infrastructure-prospero-agent-lifecycle.md) (component split)

## Context

caliban now runs each agent session under a **permission posture** chosen per session:

- `supervised`: a permission prompt goes to a human, who answers it.
- `unattended`: the session runs under a bypass profile with no human in the loop.

The default is `supervised`, so the system fails closed. In Kubernetes the posture a human chose travels on the `CalibanTask` CR. prospero reads it and puts it on the agent's `SpawnSpec`. The operator owns the CRD, so it owns the field and its admission rules.

Running unattended is a privileged act. prospero enforces this at its own API: only an `admin`-scoped token may ask prospero for an unattended session. That check does not cover the cluster. Anyone with `create` on `calibantasks` in a namespace can write the CR directly and skip prospero entirely. The operator therefore needs its own gate, one that doesn't depend on who created the CR.

## Considered Options

1. **Trust the CR as written.** Whoever can create a task can make it unattended. This is the same as having no gate: `create` on `calibantasks` is granted far more widely than the right to run agents unsupervised.
2. **Workspace policy.** Add `Workspace.spec.agentPolicy.allowUnattended`, default false. An `unattended` task is admitted only under a Workspace that sets it. The authority is write access to Workspaces, which is already the privileged object that holds providers, credentials references and egress.
3. **Admission-time identity check.** A ValidatingAdmissionPolicy (CEL on `request.userInfo`) restricts which principals may create `unattended` tasks. This is precise, but it ships in helm-charts, needs a group or ServiceAccount convention the charts don't define yet, and does nothing for tasks created before the policy existed.

## Decision

We will adopt option 2, and leave option 3 as a possible addition later.

1. `CalibanTask.spec.task.permissionPosture` is an optional enum, `supervised | unattended`. Unset means `supervised`.
2. `unattended` is admitted only when the referenced Workspace sets `agentPolicy.allowUnattended: true`. A missing policy, or a pin made before the field existed, does not permit it.
3. The check runs **before pinning**. A denied task is not pinned, is marked `Failed` with reason `PostureNotPermitted` and a Warning Event, and is re-checked on a short interval. Allowing it on the Workspace, or editing the task back to `supervised`, recovers it; the operator never silently downgrades it.
4. The policy is pinned into `status.resolvedWorkspace` with the rest of the Workspace config, as with every other part of the Workspace (ADR 0004). A pinned task is checked against its pin on every reconcile. Revoking `allowUnattended` does not disturb a task that is already running. Editing a running task's spec to `unattended` fails it if the pin doesn't allow that.
5. The posture is visible. `status.permissionPosture` records the posture a task was admitted with, and appears as a printer column. A Normal `UnattendedAdmitted` Event names the Workspace whose policy allowed it.
6. The operator does not hand the posture to caliband. prospero maps it onto `SpawnSpec.permission_posture` (ADR 0005).

## Consequences

**Positive**

- A task can't run unattended unless a Workspace author allowed it, however the task was created. prospero's `admin` scope and this gate cover the two ways in.
- Unattended tasks show up in `kubectl get calibantasks` (the `Posture` column) and in Events.
- Uses the existing resolve-and-pin model; no new admission infrastructure.

**Negative**

- The gate is only as strong as Workspace RBAC. Anyone who can edit a namespace's Workspace can allow unattended tasks there. Deployments must treat Workspace write access as privileged.
- Revoking `allowUnattended` only affects new admissions; a running unattended task keeps its pin until it finishes or is deleted.
- Within a Workspace that allows it, the gate doesn't limit which principals may create unattended tasks.

**Revisit if**

- Tenants need unattended access limited to specific users within a Workspace. At that point, add the admission-time identity check (option 3) in helm-charts.
- Revocation must stop running tasks, not just block new ones.
