# CRD reference — `Workspace` and `CalibanTask`

Both custom resources live in group **`caliban.caliban-ai.dev`**, version
**`v1alpha1`**, and are **namespaced** with a status subresource.
`v1alpha1` means the shape may change without conversion webhooks
([ADR 0001](adr/0001-kube-rs-stack-and-calibantask-crd.md)).

The Rust types in `src/crd.rs` and `src/workspace.rs` are the source of truth;
`deploy/crd/{calibantask,workspace}.yaml` is generated from them and a test
fails if the committed YAML drifts. See [Regenerating the CRDs](#regenerating-the-crds).

| Kind | Plural | Short name | Printer columns |
|---|---|---|---|
| `Workspace` | `workspaces` | `cws` | `Phase`, `Age` |
| `CalibanTask` | `calibantasks` | `ctask` | `Phase`, `Posture`, `Age` |

Runnable samples: [`deploy/samples/`](../deploy/samples/).

---

## `Workspace`

Durable, shared config a `CalibanTask` references by name: git sources, named
model providers (with their credential references), workspace-wide env, egress
and agent policy. The operator is the **sole reader of provider credential
Secrets** — a client such as prospero only ever sees the by-name reference
([ADR 0004](adr/0004-workspace-crd-and-resolve-and-pin.md)).

The `Workspace` controller provisions nothing. It validates the spec and writes
`status`.

### `spec`

| Field | Type | Required | Default | Notes |
|---|---|---|---|---|
| `displayName` | string (min 1) | **yes** | — | Dashboard label. |
| `sources[]` | list | no | `[]` | Git checkouts. A workspace may be registered bare and gain sources later. |
| `providers[]` | list (min 1) | **yes** | — | Named providers a task can bind to. |
| `defaultProvider` | string | no | — | Implicit when exactly one provider is defined. |
| `env[]` | `{name, value}` | no | `[]` | Non-secret env injected into every agent pod. Never put credentials here — use a provider `credentialsRef`. |
| `isolation` | object | no | — | Default isolation for agents in this workspace; a task's `spec.isolation` overrides it. |
| `egress` | object | no | — | Unset keeps allow-all egress. A task **cannot** override it. |
| `agentPolicy` | object | no | — | What agents here may do. A task **cannot** override it. |

`sources[]` entries:

| Field | Type | Required | Default | Notes |
|---|---|---|---|---|
| `name` | string (min 1) | **yes** | — | Matches caliband's workspace source name. |
| `repo` | string (min 1) | **yes** | — | Git remote to clone. |
| `ref` | string | no | `main` | **Branch or tag only** — `git clone --depth 1 --branch` rejects a raw commit SHA. |
| `path` | string (min 1) | **yes** | — | Absolute checkout path. Must be a directory *strictly* under the operator's workspace root (default `/work`) and distinct from every other source's path, or the `Workspace` goes `Failed`. |

`providers[]` entries:

| Field | Type | Required | Notes |
|---|---|---|---|
| `name` | string (min 1) | **yes** | Unique within the workspace. |
| `kind` | string (min 1) | **yes** | `anthropic`, `openai`, `google`/`gemini` take their exact env names from `caliban-contract`. Other kinds (`azure`, `bedrock`, `vertex`, …) get `<KIND>_BASE_URL` / `<KIND>_API_KEY`, with `azure`/`azure-openai` mapped to `AZURE_OPENAI_*`. |
| `baseUrl` | string | no | Projected under the kind's native base-URL env var. |
| `model` | string | no | Recorded in the pin. **Not** projected as env — the model reaches the worker through caliband's `SpawnSpec`. |
| `credentialsRef` | `{secretName, key}` | no | Omit for keyless providers (e.g. a local `openai`-compatible endpoint). Reaches the pod as a `secretKeyRef`; the operator never inlines the value. |

Base URL and API key are projected under **provider-native** env names, because
caliban implements no `CALIBAN_*` namespace for them — it reads
`OPENAI_BASE_URL`, `ANTHROPIC_API_KEY` and so on. The `kind` is additionally
projected as `CALIBAN_PROVIDER`, not as a provider selector (selection travels in
caliband's `SpawnSpec`) but because it is the documented input to a
user-configured `apiKeyHelper`.

`egress` restricts agent-pod egress to DNS plus the listed destinations,
replacing the default allow-all rule. An **empty `allow` list is DNS-only**.

| Field | Type | Required | Default | Notes |
|---|---|---|---|---|
| `egress.allow[].cidr` | string (min 1) | **yes** | — | IPv4 or IPv6 CIDR, e.g. `192.0.2.0/24`, `2001:db8::/32`. A malformed CIDR or a prefix longer than the address fails the `Workspace`. |
| `egress.allow[].ports[]` | list of int | no | `[]` | TCP ports. Empty allows every port. Anything outside `1-65535` fails the `Workspace`. |

When you set `egress`, remember to include the workspace's **git remotes**, its
**model provider endpoints**, and **gonzalod** if a task uses `spec.state`.

`agentPolicy`:

| Field | Type | Default | Notes |
|---|---|---|---|
| `agentPolicy.allowUnattended` | bool | `false` | Admits tasks whose `permissionPosture` is `unattended`. **Whoever can edit this Workspace controls it — treat Workspace write access as privileged** ([ADR 0006](adr/0006-unattended-permission-posture-authorized-by-workspace-policy.md)). |

### `status`

| Field | Notes |
|---|---|
| `phase` | `Pending` (not yet reconciled) → `Ready` (all providers and credential Secrets resolve) or `Failed`. |
| `message` | Why it failed, e.g. `provider 'planner': secret 'anthropic-key' key 'api-key' not found`. |
| `observedGeneration` | The `.metadata.generation` this status reflects. |
| `conditions[]` | Standard Kubernetes conditions. |

Validation runs in this order, first problem wins: source paths (under root,
distinct) → egress CIDRs and ports → duplicate provider names → a resolvable
`defaultProvider` → an existing Secret key for every `credentialsRef`.

The controller deliberately does **not** watch Secrets — that would need
cluster-wide `list`/`watch` on every Secret. Recovery rides the requeue instead:
a `Ready` workspace is re-checked every **300s**, a `Pending` or `Failed` one
every **15s**, so creating a missing Secret unblocks it within seconds.

---

## `CalibanTask`

One agent session: a reference to a `Workspace`, the prompt, and per-run
overrides. The operator reconciles it into an agent-sandbox `Sandbox`, a
token-less ServiceAccount and a NetworkPolicy
([ADR 0002](adr/0002-reconcile-calibantask-to-sandbox.md)).

### `spec`

| Field | Type | Required | Default | Consumed by |
|---|---|---|---|---|
| `workspaceRef.name` | string (min 1) | **yes** | — | operator |
| `providerRef` | string | no | the workspace's `defaultProvider`, else its sole provider | operator |
| `task.prompt` | string (min 1) | **yes** | — | prospero |
| `task.agentType` | string | no | — | carried on the CR; the operator does not read it |
| `task.interactive` | bool | no | `false` | prospero (`SpawnSpec.interactive`); the operator only carries it |
| `task.permissionPosture` | enum | no | `supervised` | operator (admission gate) + prospero |
| `model.routerConfigRef` | string | no | — | operator |
| `state.gonzaloEndpoint` | string | no | — | operator |
| `state.mode` | string: `remote` \| `local` | no | `remote` when `gonzaloEndpoint` is set | operator |
| `state.tokenRef` | `{secretName, key}` | no | — | operator |
| `isolation.runtimeClass` | string | no | the workspace default | operator (pod `runtimeClassName`) |
| `isolation.worktrees` | string | no | — | declared, not yet consumed |
| `resources.class` | string | no | — | declared; `SandboxTemplate` selection is deferred |
| `lifecycle.idleTimeout` | string | no | — | declared; idle→pause mapping is deferred |
| `lifecycle.onDelete` | string: `checkpoint` \| `delete` | no | — | declared; the drain finalizer is deferred |
| `tools[]` | list of string | no | — | declared, not yet consumed |

Only `permissionPosture` carries an `enum` in the generated schema, so only it is
rejected by the API server on a typo. `state.mode` is a plain string the operator
validates at reconcile time (failing the task with `InvalidState`); the strings in
the deferred `isolation.worktrees`, `resources.class` and `lifecycle.*` fields are
unconstrained and unread.

`spec.isolation` is a **per-run override that wins over the workspace default**.
That is exactly why `egress` and `agentPolicy` are Workspace-level fields and
not part of the shared isolation block — a task must not be able to widen a
restriction the workspace imposed.

#### `permissionPosture`

| Value | Meaning |
|---|---|
| `supervised` | A permission prompt goes to a human, who answers it. The default, and what an unset field means — the system fails closed. |
| `unattended` | The session runs under an authorized bypass profile with no human in the loop. |

`unattended` is admitted **only** when the referenced `Workspace` sets
`agentPolicy.allowUnattended: true`. A missing policy — including a pin made
before the field existed — does not permit it. The check runs **before pinning**,
so a denied task is never pinned to the policy that denied it; allowing it on the
Workspace, or editing the task back to `supervised`, recovers it. The operator
never silently downgrades a posture.

A pinned task is re-checked against **its pin** on every reconcile. Revoking
`allowUnattended` therefore does not disturb a task already running, but editing
a running task's spec to `unattended` fails it if the pin does not allow it.

The operator does not hand the posture to caliband. prospero maps it onto
`SpawnSpec.permission_posture`, reading `status.permissionPosture` — the posture
the operator **admitted** — not the spec field.

#### `state` (gonzalo persistence)

`spec.state` becomes caliban's storage settings by environment
(`CALIBAN_STORAGE_SUBSTRATE`, `CALIBAN_STORAGE_REMOTE_URL`,
`CALIBAN_STORAGE_REMOTE_TOKEN_ENV`), so the operator never authors caliban's
settings file. The CRD's `local` is caliban's `fs`. Requires a caliban build
with environment overrides for those settings.

A `tokenRef` projects the gonzalod bearer token into the pod as `GONZALO_TOKEN`
from the named Secret; only the variable's *name* appears in the pod spec. Use a
principal with read/write on the gonzalod `caliban` namespace **only** — no write
on `fleet` or `fleet-audit`.

These combinations fail the task with reason `InvalidState`: an unknown `mode`;
`mode: remote` without `gonzaloEndpoint`; `mode: local` with a `gonzaloEndpoint`
it would never use; a `tokenRef` without remote storage; a `tokenRef` whose
Secret or key is missing.

### `status`

| Field | Owner | Notes |
|---|---|---|
| `phase` | operator | See below. |
| `calibandEndpoint` | operator | `<serviceFQDN>:<calibandPort>`, set only once the Sandbox is `Ready`. |
| `sandboxRef.name` | operator | `<task>-sbx`. |
| `resolvedWorkspace` | operator | The pinned config (sources, the single resolved provider, env, isolation, egress, agentPolicy). Set once at admission; later `Workspace` edits do not re-pin a running task. |
| `permissionPosture` | operator | The posture the task was **admitted** with. The only posture prospero acts on. |
| `conditions[]` | **split** | A map-list keyed by `type`. The operator owns `Ready`; prospero owns `AgentsSettled`. Each applies only its own entries under its own field manager, so neither clobbers the other ([ADR 0005](adr/0005-operator-infrastructure-prospero-agent-lifecycle.md)). |
| `checkpointRef` | — | Declared; checkpointing is deferred. |

The operator writes status with **server-side apply** under field manager
`caliban-operator`. A field it owns but omits is removed, so a cleared value is
omitted rather than serialized as `null`.

#### Phases

| Phase | When |
|---|---|
| `Pending` | Observed before its first successful apply — no Sandbox yet. |
| `Provisioning` | The Sandbox is applied but has not reported `Ready`. |
| `Running` | agent-sandbox reports the Sandbox `Ready=True` — the pod passed its readiness probe on caliband's control port and the Service is ready. |
| `Draining` | Declared; not yet produced (the drain/checkpoint lifecycle is deferred). |
| `Completed` | prospero's `AgentsSettled=True` with reason `Succeeded`. |
| `Failed` | prospero's `AgentsSettled=True` with reason `Failed`, or a terminal operator-side failure (below). |

A `serviceFQDN` alone is **not** readiness: it appears as soon as the Service
object exists, before caliband has bound its port. `Running` gates on
agent-sandbox's own `Ready` condition instead.

Terminal phases come from prospero, because infrastructure cannot tell "agent
running" from "agent finished, pod still up" — only caliband's agent list knows,
and the operator is never a caliband client. Without prospero deployed, a
finished task stays `Running`.

#### `Ready` condition reasons (operator-owned)

| `reason` | `status` | Meaning and recovery |
|---|---|---|
| `Running` | `True` | caliband is up and attachable. |
| `SandboxNotReady` | `False` | agent-sandbox reported not-ready; `message` carries its detail (e.g. "Pod is Running but not Ready"). |
| `Completed` | `False` | Agents finished successfully; the task is done, so not Ready. |
| `AgentsFailed` | `False` | One or more agents failed or crashed. |
| `InvalidName` | `False` | The task name is too long for its Sandbox's Service name (`<task>-sbx`, max 63 chars). Names are immutable — recreate under a shorter name. The operator stops retrying. |
| `WorkspaceUnresolved` | `False` | No such `Workspace`, a `Failed` one, or an unresolvable/ambiguous `providerRef`. `message` names which. Re-checked every 30s; fixing the `Workspace` recovers the task. |
| `InvalidState` | `False` | `spec.state` is malformed or its token Secret is missing. Re-checked every 30s. |
| `PostureNotPermitted` | `False` | `unattended` without `agentPolicy.allowUnattended`. Re-checked every 30s. |

`AgentsSettled` reasons are prospero's: `Succeeded`, `Failed`, or `AgentsActive`
(not settled — an idle interactive task stays unsettled).

A task whose `Workspace` exists but is still `Pending` is **not** failed: it is
not pinned either, and the reconcile simply requeues in 10s to re-check once the
`Workspace` controller has validated it.

### Events

`kubectl describe calibantask <name>` shows what the logs would otherwise hold.
Notes are truncated at 1 kB; publishing is best-effort and never fails or delays
a reconcile.

| Reason | Type | When |
|---|---|---|
| `PhaseChanged` | Normal | The phase moves, e.g. `Provisioning → Running`. |
| `UnattendedAdmitted` | Normal | A task was admitted `unattended`; the note names the Workspace whose policy allowed it. |
| `ReconcileError` | Warning | A reconcile returned an error. |
| `InvalidName` / `WorkspaceUnresolved` / `InvalidState` / `PostureNotPermitted` | Warning | The task was marked `Failed`; the Event reason is the failure's own reason code. |

---

## What one `CalibanTask` creates

| Object | Name | Notes |
|---|---|---|
| `Sandbox` (`agents.x-k8s.io/v1beta1`) | `<task>-sbx` | The caliband pod, its workspace PVC, and a headless Service giving the stable `serviceFQDN`. |
| `ServiceAccount` | `<task>-sa` | `automountServiceAccountToken: false` and **no `Role`/`RoleBinding`** — the pod holds zero API permissions. The token-less SA *is* the RBAC posture. |
| `NetworkPolicy` | `<task>-netpol` | `policyTypes: [Ingress, Egress]`, selecting the pod by the labels the operator propagates. |

All three carry a controller `OwnerReference` to the task, so deleting the task
cascades, and are applied with server-side apply so re-reconciles converge on
one object. Deleting a task tears its Sandbox down immediately, **without**
checkpointing in-flight agents (the drain finalizer is deferred).

The pod runs **every** container — the git-clone init container included — as one
uid/gid (default `10001`, caliban's `app` user) with `runAsNonRoot: true` and
`fsGroup` set to the same gid, so the clone step and the agent cannot drift apart
and a non-root clone succeeds whatever mode the volume provisioner gave the
volume root. See [Deploying](deploying.md#pod-identity-and-the-workspace-volume)
for the one case this does not repair.

The reconcile re-runs on every change to the task and to the Sandboxes it owns,
so a Sandbox becoming `Ready` reconciles the task at once. The **300s** periodic
resync is only a safety net against missed events.

---

## Regenerating the CRDs

The committed YAML is generated, never hand-edited. A `*_is_in_sync` test fails
if it drifts from the Rust types:

```sh
cargo run --bin crdgen calibantask > deploy/crd/calibantask.yaml
cargo run --bin crdgen workspace   > deploy/crd/workspace.yaml
cargo test --workspace
```

The `Sandbox` type in `src/sandbox.rs` is a **read/write view** of a CRD
agent-sandbox owns, not a schema of record. `crdgen` never emits it and no test
drift-guards it; fields the operator omits are pruned by the API server's
structural schema.

After a CRD change, the copies in the `caliban-crds` Helm chart need the same
update — that chart names `deploy/crd/*.yaml` here as its source of truth.
