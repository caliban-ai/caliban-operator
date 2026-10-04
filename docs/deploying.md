# Deploying and configuring caliban-operator

The operator is a controller: it watches `Workspace` and `CalibanTask` custom
resources cluster-wide and creates `Sandbox`, `ServiceAccount` and
`NetworkPolicy` objects. It serves no inbound traffic, so it has no Service,
Ingress or port.

This page is the reference for the operator's own **configuration surface** and
the cluster **prerequisites** it assumes. The Helm charts themselves live in
[caliban-ai/helm-charts](https://github.com/caliban-ai/helm-charts); for chart
values, see that repo.

## Prerequisites

1. **agent-sandbox** — the `agents.x-k8s.io/v1beta1` `Sandbox` CRD *and* its
   controller must be present. This is a cluster-level prerequisite, like a CNI
   or a RuntimeClass, normally installed by the cluster admin. The operator's
   `Sandbox` view is written against
   [agent-sandbox **v0.5.0**](https://agent-sandbox.sigs.k8s.io); helm-charts
   vendors that version as `charts/agent-sandbox` and the umbrella bundles it by
   default. Without it, every reconcile's Sandbox apply fails and tasks stay
   `Provisioning` — a retried error, not a crash.
2. **The caliban CRDs** — `calibantasks` and `workspaces`, installed as their
   own step from the `caliban-crds` chart (Helm does not upgrade a `crds/`
   directory, so that chart keeps them in `templates/`). They are copies of
   [`deploy/crd/`](../deploy/crd/) here, which is the source of truth. Applying
   them directly works too:
   ```sh
   kubectl apply -f deploy/crd/
   ```
   **Upgrade the CRDs before the operator.** Against an older CRD, a field the
   operator relies on — `spec.task.permissionPosture`, `spec.agentPolicy`,
   `spec.model.name` — is either rejected outright (`kubectl apply` decodes
   strictly: `unknown field "spec.task.permissionPosture"`) or silently pruned
   by the structural schema, so an `unattended` task would read back as
   `supervised`.
3. **Lease RBAC, before the operator image** — leader election is on by default,
   so the operator needs `coordination.k8s.io/leases` in its own namespace. The
   chart's lease `Role` is harmless against an older operator, so upgrading the
   chart first is always safe; the reverse order is not. See
   [Leader election](#leader-election).
4. **A session-plane TLS Secret and token Secret** — see
   [Session plane](#session-plane) below.
5. **A default StorageClass** — each task's workspace PVC is created with no
   `storageClassName`, so it binds the cluster default (deliberately
   cluster-agnostic).

## Kubernetes versions

The operator compiles against the **`v1_32`** `k8s-openapi` API surface (the
lowest the kube 4.0 / k8s-openapi 0.28 pairing offers). It uses only
long-stable `PodSpec`, `NetworkPolicy` and `apiextensions.k8s.io/v1` fields, so
it is forward-compatible across the usual skew. The original target was k3s
v1.31 ([ADR 0001](adr/0001-kube-rs-stack-and-calibantask-crd.md)); it currently
runs on k3s v1.36. No chart declares a `kubeVersion` floor, so treat this as a
record of what has been run, not an enforced constraint.

`NetworkPolicy` is only enforced if the cluster's CNI implements it. On a CNI
that ignores it, the per-task policy is applied and inert — the isolation
posture is silently absent.

## Installing

CRDs first, then the operator. Via the charts
([caliban-ai/helm-charts](https://github.com/caliban-ai/helm-charts), published
to the OCI registry `oci://ghcr.io/caliban-ai/charts`, not an HTTP chart repo —
there is no `helm repo add`):

```sh
helm install caliban-crds     oci://ghcr.io/caliban-ai/charts/caliban-crds
helm install caliban-operator oci://ghcr.io/caliban-ai/charts/caliban-operator
```

(If the registry refuses an anonymous pull, the GHCR package is still private —
`helm registry login ghcr.io` first.)

Or install the `caliban-system` umbrella chart, which can bring agent-sandbox,
gonzalo and prospero with it. Either way, keep environment-specific values in a
separate private overlay (`-f private-values.yaml`); the charts ship neutral,
cluster-agnostic defaults only.

Then confirm it is up and try a task:

```sh
kubectl get deploy -l app.kubernetes.io/name=caliban-operator   # 1/1 available
kubectl apply -f deploy/samples/workspace.yaml
kubectl get workspaces          # wait for Phase: Ready
kubectl apply -f deploy/samples/calibantask.yaml
kubectl get calibantasks -A     # Phase, Posture
```

The samples target namespace `team-a` and reference Secrets that do not exist by
default, so the `Workspace` will report `Failed` with the missing Secret named in
`status.message` until you create them. That is the intended first signal.

## Configuration

Every setting is an environment variable on the controller container; there are
no flags and no config file. Unset means the default below.

### Images and the agent pod

| Variable | Default | Notes |
|---|---|---|
| `CALIBAND_IMAGE` | `ghcr.io/caliban-ai/caliban:latest` | The caliband image run in each Sandbox. **Pin a real tag in production** — `latest` makes an agent pod's version unreproducible. |
| `CALIBAN_GIT_IMAGE` | `alpine/git:latest` | The git-clone init container that populates the workspace. |
| `CALIBAN_WORKSPACE_ROOT` | `/work` | Workspace mount path in the pod. Every `Workspace` source `path` must sit strictly under it. |
| `CALIBAN_WORKSPACE_STORAGE` | `10Gi` | Requested size of each task's workspace PVC. |
| `CALIBAN_AGENT_UID` | `10001` | uid every container in the sandbox pod runs as. |
| `CALIBAN_AGENT_GID` | `10001` | gid for the same, and the pod's `fsGroup`. |

Note what is **not** here: caliban's own permission and extension behaviour is
not operator configuration. It is set per workspace, as typed
[`Workspace.spec.agentPolicy`](crds.md#agentpolicy--what-agents-in-this-workspace-may-do)
fields, which the operator projects into each agent pod as `CALIBAN_*`
environment (`CALIBAN_DEFAULT_PERMISSION_MODE`, `CALIBAN_AUTO_ALLOW`,
`CALIBAN_NO_PERMISSIONS`, `CALIBAN_NO_MCP`, `CALIBAN_NO_HOOKS`,
`CALIBAN_NO_SKILLS`, `CALIBAN_NO_SUB_AGENT`,
`CALIBAN_STRICT_KNOWN_MARKETPLACES`, `CALIBAN_ENABLED_PLUGINS`,
`CALIBAN_BLOCKED_MARKETPLACES`, `CALIBAN_PARALLEL_TOOL_LIMIT`). Those values win
over a `Workspace.spec.env` entry of the same name, and the three unsupervised
ones are gated by `agentPolicy.allowUnattended`. There is deliberately no
cluster-wide default for them: the authority is write access to a `Workspace`.

### Ports

| Variable | Default | Notes |
|---|---|---|
| `CALIBAND_PORT` | `8443` | caliband's TCP+TLS control port; also the readiness/startup probe target and the ingress port the NetworkPolicy opens. |
| `CALIBAN_AGENT_PORT_BASE` | `7100` | Bottom of caliband's per-agent stream port window. Passed to caliband *and* opened in the NetworkPolicy from one setting, so the two cannot drift. |
| `CALIBAN_AGENT_PORT_END` | `7999` | Top of that window, inclusive. |

The operator validates these at startup and **refuses to start** on a port
outside `1-65535`, an empty window (`BASE > END`), or a `CALIBAND_PORT` that
falls inside the agent window. The error names the offending variable, rather
than letting a NetworkPolicy the API server rejects fail every reconcile.

### Session plane

caliband serves TLS and requires a bearer token. Both come from pre-existing
Secrets in the task's namespace, mounted or projected by the operator.

| Variable | Default | Notes |
|---|---|---|
| `CALIBAN_SESSION_TLS_SECRET` | `caliban-session-plane-tls` | Serving-cert Secret with keys `tls.crt`, `tls.key`, `ca.crt`. Mounted read-only at `/etc/caliband/tls`. |
| `CALIBAN_SESSION_TOKEN_SECRET` | `caliban-session-plane-token` | Bearer-token Secret. |
| `CALIBAN_SESSION_TOKEN_KEY` | `token` | Key within the token Secret. |
| `CALIBAN_SESSION_SERVER_NAME` | `caliband` | The name the serving cert is verified against. **Must equal that cert's SAN** and the client's TLS SNI, or the workers caliband spawns fail the handshake and their status reports are silently dropped. |

The token is never an argv value — it reaches the pod from the Secret as an
environment variable, so it stays out of `kubectl get pod -o yaml` and the
process table. The `ca.crt` is handed to caliband so it can pass a trust anchor
down to the workers it spawns.

### Agent pod resources

Defaults give every agent pod **Burstable** QoS: requests, no limits — so a pod
is not first in line for eviction and is admitted under a `ResourceQuota`,
without a guessed ceiling OOM-killing an agent. An explicitly **empty** value
clears a default rather than only overriding it.

| Variable | Default | Notes |
|---|---|---|
| `CALIBAND_CPU_REQUEST` | `250m` | |
| `CALIBAND_MEMORY_REQUEST` | `512Mi` | |
| `CALIBAND_CPU_LIMIT` | unset | |
| `CALIBAND_MEMORY_LIMIT` | unset | |

The git-clone init container gets the same values. A pod's effective request is
the max of any init container and the sum of its app containers, so this costs
nothing at scheduling while still admitting the init step under quota.

### Cluster topology and network reach

| Variable | Default | Notes |
|---|---|---|
| `CALIBAN_CLUSTER_DOMAIN` | `cluster.local` | The cluster's DNS domain, used to build caliband's advertise host `<sandbox>.<namespace>.svc.<domain>`. A blank value keeps the default; a trailing root dot is trimmed. |
| `CALIBAN_INGRESS_POD_SELECTOR` | empty | `key=value,key2=value2`. Narrows the NetworkPolicy's ingress peer to matching pods. Empty admits **every** pod in the peer namespace. A malformed entry fails startup rather than silently widening or dropping the rule. |
| `CALIBAN_INGRESS_NAMESPACE` | the task's own | Namespace allowed to reach caliband, matched on its `kubernetes.io/metadata.name` label. |

Unconfigured, ingress to caliband is open to every pod in the task's own
namespace — the historical default-deny-everything-else posture. In a real
deployment, narrow it to prospero: set `CALIBAN_INGRESS_NAMESPACE` to prospero's
namespace and `CALIBAN_INGRESS_POD_SELECTOR` to its pod labels.

### Observability

| Variable | Default | Notes |
|---|---|---|
| `RUST_LOG` | `info` | `tracing-subscriber` env filter. An unset value yields `info`, not silence. |
| `CONTROLLER_POD_NAME` | unset | Recorded as the `instance` on every Event the operator publishes. Set it from `fieldRef: metadata.name` so Events name the replica that wrote them. |

## RBAC the operator needs

The operator needs a **cluster-scoped** role — it watches both CRs across all
namespaces. This is the verb set the charts grant:

| apiGroups | resources | verbs |
|---|---|---|
| `caliban.caliban-ai.dev` | `calibantasks` | get, list, watch, update, patch |
| `caliban.caliban-ai.dev` | `calibantasks/status` | get, update, patch |
| `caliban.caliban-ai.dev` | `workspaces` | get, list, watch |
| `caliban.caliban-ai.dev` | `workspaces/status` | get, update, patch |
| `""` | `secrets` | get |
| `agents.x-k8s.io` | `sandboxes` | get, list, watch, create, update, patch, delete |
| `""` | `serviceaccounts` | get, list, watch, create, update, patch, delete |
| `networking.k8s.io` | `networkpolicies` | get, list, watch, create, update, patch, delete |
| `events.k8s.io` | `events` | create, patch |
| `coordination.k8s.io` | `leases` | get, list, watch, create, update, patch, delete |

The `leases` grant is **required by default**, because leader election is on by
default — see [Leader election](#leader-election). It only needs to be a
namespaced `Role` in the operator's own namespace, since the lease lives there.

Notes on the shape of that set:

- **`get` on `secrets`, never `list` or `watch`.** The operator reads a Secret
  only by the name a `Workspace` references, to answer "does this key exist". It
  never lists or caches Secret data, which is why the `Workspace` controller
  re-checks on a requeue instead of watching Secrets.
- **No `create`/`delete` on `calibantasks` or `workspaces`.** Clients such as
  prospero own CR lifecycle; the operator only observes them and writes status.
- `events.k8s.io` is required for the Events in
  [the CRD reference](crds.md#events). Without it, publishing logs a warning and
  the reconcile proceeds — Events are best-effort.
- **prospero needs `patch` on `calibantasks/status`** of its own to write the
  `AgentsSettled` condition. Without it, tasks never reach a terminal phase.
  That grant lives in prospero's chart, not the operator's.

## Leader election

The operator elects a leader through a `coordination.k8s.io` Lease before
running either controller, and **it is on by default**. A single replica simply
acquires the lease immediately; the point of the default is that raising
`replicaCount` can never silently leave two controllers force-applying the same
objects.

| Variable | Default | Notes |
|---|---|---|
| `CALIBAN_LEADER_ELECTION` | `true` | Accepts `1`/`true`/`yes`/`on` and `0`/`false`/`no`/`off`; **anything else fails startup** rather than being guessed. Set it false only for a single-replica install that manages exclusivity itself. |
| `CALIBAN_LEASE_NAME` | `caliban-operator` | The Lease object the replicas contend for. Must not be blank. |
| `CALIBAN_LEASE_NAMESPACE` | the operator's own namespace | Read from `/var/run/secrets/kubernetes.io/serviceaccount/namespace` when unset. The chart sets it explicitly to the release namespace. |
| `CALIBAN_LEASE_DURATION_SECONDS` | `15` | How long the lease is held before it may be taken over. |
| `CALIBAN_LEASE_GRACE_SECONDS` | `5` | How long before expiry the holder starts renewing. **Must be below the duration**, or the holder could lose the lease while still believing it holds it — startup rejects that. |
| `POD_NAME` | the pod's `HOSTNAME`, else `caliban-operator` | The identity this replica claims the lease under. It **must differ per replica**, or two replicas look like the same holder and both proceed. The chart sets it from `fieldRef: metadata.name`; the hostname fallback only matters outside a cluster. Note this is a **different** variable from `CONTROLLER_POD_NAME`, which only labels Events. |

Operationally:

- **Losing the lease stops the controllers and returns the process to standby**
  rather than exiting, so an apiserver blip costs a pause instead of a crash
  loop. A replica that never wins simply waits, doing nothing.
- A clean shutdown **releases** the lease rather than waiting out the duration,
  so failover is prompt.
- Outside a cluster, there is no namespace file: either set
  `CALIBAN_LEASE_NAMESPACE` or `CALIBAN_LEADER_ELECTION=false`. The startup
  error says exactly this.
- The lease is applied under field manager `caliban-operator`, like everything
  else the operator writes.

Running with `CALIBAN_LEADER_ELECTION` on but **without** the `leases` RBAC above
is the one new way a previously-working install can break on upgrade: the
operator starts, fails to acquire, and never reconciles. **Upgrade the chart
before the operator image** — the chart's lease Role is harmless against an
operator that predates leader election (an unused Role and env it ignores),
whereas the reverse order fails on lease RBAC. If you keep election off, keep
`replicaCount: 1`: with election off, nothing stops two replicas from
force-applying the same objects.

In the chart this is the `leaderElection` block (`enabled`, `leaseName`,
`durationSeconds`, `graceSeconds`), on by default. Its lease `Role` is gated
behind *both* `leaderElection.enabled` and `rbac.create`, so `rbac.create=false`
with election on is the combination to watch.

The operator's own Deployment should run as uid `10001` with
`runAsNonRoot: true`, `allowPrivilegeEscalation: false`,
`readOnlyRootFilesystem: true` and all capabilities dropped. It writes nothing
to disk, so a read-only root filesystem is safe. See
[the container image](container.md).

## Pod identity and the workspace volume

Every container in a sandbox pod — the git-clone init container included — runs
as `CALIBAN_AGENT_UID`/`CALIBAN_AGENT_GID` (default `10001`, matching the `app`
user in caliban's image), with `runAsNonRoot: true` and `fsGroup` set to the
same gid.

The uid sits on the **pod**, not on each container, so there is nothing to keep
in sync between the clone step and the agent. `fsGroup` is what makes a non-root
clone work on any volume rather than by luck: kubelet applies the group to the
mounted workspace with `g+rwX`, so the clone does not depend on the mode the
provisioner gave the volume root. `runAsNonRoot` turns a misconfigured uid 0
into an admission failure instead of a silent return to root-owned sources.

`CALIBAN_AGENT_UID` must match the caliband image's user. caliban's image bakes
`HOME` and an `XDG_RUNTIME_DIR` owned by uid `10001`; a different uid cannot
write them.

**One case this does not repair:** the clone is idempotent and skips a source
whose `<path>/.git` already exists, so a workspace volume cloned by an older,
root-running init container still holds root-owned sources. The init container
is now non-root and cannot chown them back, and git compares the owner uid, so
it refuses the checkout even though `fsGroup` made the files writable. Recreate
the task, or delete its workspace PVC, to re-clone as the agent.

## Troubleshooting

Start with the task's own status and Events — the failure reason is always on
the object:

```sh
kubectl get calibantask <name> -o jsonpath='{.status.phase} {.status.permissionPosture}{"\n"}'
kubectl describe calibantask <name>      # conditions + Events
kubectl get workspace <name> -o jsonpath='{.status.phase}: {.status.message}{"\n"}'
```

| Symptom | Likely cause |
|---|---|
| Operator pod `CrashLoopBackOff` immediately, log names a variable | Startup validation rejected a port, uid or lease setting. Fix the named variable. |
| Operator runs but reconciles nothing, no errors on any task | It is a non-leader standby, or it cannot acquire the lease. Check the log for "contending for the leader lease" / "acquired the leader lease", then `kubectl get lease caliban-operator`. A `forbidden` on leases means the lease RBAC is missing. |
| Two replicas both reconciling | `CALIBAN_LEADER_ELECTION=false` with `replicaCount > 1`, or both replicas sharing one `POD_NAME` so they look like the same lease holder. |
| Operator fails at startup naming `CALIBAN_LEASE_NAMESPACE` | Running outside a cluster, where the ServiceAccount namespace file does not exist. Set that variable or `CALIBAN_LEADER_ELECTION=false`. |
| `Workspace` stuck `Failed` | Read `status.message` — a source path outside the workspace root, a duplicate path, a bad egress CIDR, a duplicate provider name, an unresolvable `defaultProvider`, or a missing credential Secret key. |
| Task `Failed`, `WorkspaceUnresolved` | The `Workspace` is missing or `Failed`, or `providerRef` names no provider. Re-checked every 30s. |
| Task `Failed`, `PostureNotPermitted` | Unsupervised power without `agentPolicy.allowUnattended`. `message` names the setting — the task's `permissionPosture: unattended`, or the Workspace's own `permissionMode: dontAsk`/`bypassPermissions`, `autoAllow` or `noPermissions`. |
| `kubectl apply` rejects `unknown field "spec.agentPolicy.permissionMode"` (or `spec.model.name`) | The installed CRDs predate the field. Upgrade the `caliban-crds` chart. |
| An `agentPolicy` setting seems ignored | It was unset, not `false` — unset leaves caliban's own default. Also check the task's pin: `status.resolvedWorkspace.agentPolicy` is what the running task was admitted with, and editing the Workspace does not re-pin it. |
| A task runs the wrong model | `spec.model.name` is pinned at admission. Read `status.resolvedWorkspace.provider.model`; editing the task afterwards does not move a running one. |
| Task `Failed`, `InvalidName` | The task name is too long for its `<task>-sbx` Service. Names are immutable — recreate with a shorter one. |
| Task stuck `Pending` | No Sandbox was created. Check the operator's RBAC on `sandboxes.agents.x-k8s.io` and that agent-sandbox is installed. |
| Task stuck `Provisioning` | The Sandbox exists but is not `Ready`. `kubectl describe sandbox <task>-sbx` and check the pod: a pull failure on the caliband or git image, an unschedulable PVC, or caliband not binding its control port. |
| Task reaches `Running` and never leaves | Expected without prospero — the operator cannot observe agent completion. Check prospero is deployed and holds `patch` on `calibantasks/status`. |
| Agents report nothing; interactive agents never go idle | A session-plane TLS mismatch. `CALIBAN_SESSION_SERVER_NAME` must equal the serving cert's SAN. |
| Clone fails with "detected dubious ownership" | A pre-existing workspace volume with root-owned sources. Delete the PVC and recreate the task. |
| Agent cannot reach a git remote or a provider | The `Workspace` declares `spec.egress` and the destination is not in the allow-list. Remember DNS is always allowed but the destinations are not. |
