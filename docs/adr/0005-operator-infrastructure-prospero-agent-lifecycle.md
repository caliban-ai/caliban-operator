# ADR 0005 · Operator owns infrastructure, prospero owns agent lifecycle, caliban owns its contract

- **Status:** accepted
- **Date:** 2026-09-13
- **Source:** caliban-operator [#37](https://github.com/caliban-ai/caliban-operator/issues/37) design
  discussion · follow-ups: caliban [#656](https://github.com/caliban-ai/caliban/issues/656) (contract
  crate), prospero [#228](https://github.com/caliban-ai/prospero/issues/228) (`AgentsSettled`),
  helm-charts [#52](https://github.com/caliban-ai/helm-charts/issues/52) (RBAC), caliban-operator
  [#64](https://github.com/caliban-ai/caliban-operator/issues/64) (server-side apply status) · amends
  decision 6 of [ADR 0002](0002-reconcile-calibantask-to-sandbox.md)

## Context

Three components cooperate around a `CalibanTask`:

- **caliban-operator** reconciles the CR into a Sandbox, ServiceAccount, and NetworkPolicy, and derives `status` from Kubernetes objects ([ADR 0002](0002-reconcile-calibantask-to-sandbox.md)). It launches caliband by writing its pod spec ([ADR 0003](0003-caliband-launch-contract.md)). It has **no network client** for caliband.
- **prospero** dials caliband over TLS + bearer token to spawn, attach, stream, and poll agents (`list()`).
- **caliband** supervises the agents inside the sandbox.

caliban's contract with the other two is implicit and copied by hand. The operator types caliband's flag and env names as strings in `src/resources.rs`; prospero mirrors caliban's control wire types in its own `wire.rs`, deliberately not depending on `caliban-supervisor` because that crate is the whole daemon. Each drift has surfaced only at runtime, silently: provider env names caliban never read (#30), a missing CA and then a wrong verified server name that kept agents from ever reporting Idle (#32, #35), and a router-config env name nothing read (#44).

#37 exposed the next step of this pattern. A `CalibanTask` never leaves `Running` because only caliband's agent list knows an agent finished — the pod stays up. The obvious fix, having the operator poll caliband, would make the operator a **third** hand-coupled consumer of caliban's wire protocol (a TLS client, token handling, protocol version lockstep), alongside prospero, which already has all of that.

## Decision Drivers

- Minimize the number of components that speak caliban's control protocol.
- Turn contract drift from silent runtime failure into a compile-time error.
- Keep one owner per status field; no component may clobber another's writes.
- Preserve sandbox isolation (sandbox pods stay token-less).

## Considered Options

1. **Operator polls caliband** via `caliban-supervisor` or a mirrored client. Single status writer, but a third protocol consumer and a heavy daemon dependency.
2. **caliband writes its own lifecycle into the CR.** Protocol-free for everyone else, but the sandbox pod would need a Kubernetes API token and RBAC, weakening isolation.
3. **caliband exits when its agents finish** (pod `Succeeded`/`Failed`). Fully Kubernetes-native, but only fits one-shot tasks, not interactive ones.
4. **Split responsibilities, meet in the CR via server-side apply field ownership, and publish the contract** (chosen).

## Decision

We will draw the component boundaries as follows:

1. **The operator owns infrastructure.** It manages the Sandbox, pod, ServiceAccount, and NetworkPolicy, and derives infrastructure state (`Pending`/`Provisioning`/`Running`, `calibandEndpoint`, `sandboxRef`, its `Ready` condition) from Kubernetes objects only. It does **not** become a caliband client.
2. **prospero owns agent lifecycle** and is the only component that speaks caliband's control protocol. It reports agent state into the CR as an **`AgentsSettled`** condition: settled when no agent is `Spawning`/`Running`/`Idle`; reason `Failed` if any ended `Failed`/`Crashed`, else `Succeeded`.
3. **They meet in `CalibanTask.status` through server-side apply field ownership.** Each component applies only the fields it owns under its own field manager (`caliban-operator`, `prospero`); `conditions` is a map-list keyed by `type`. The operator maps `AgentsSettled` to `phase: Completed`/`Failed` without knowing the wire protocol. This amends decision 6 of ADR 0002, which derived status from the Sandbox alone.
4. **caliban owns and publishes its contract** as a thin `caliban-contract` crate with no daemon internals: a typed caliband launch builder (flags and env) and the control wire types, with an optional feature-gated client. The operator builds caliband's launch arguments from it; prospero replaces its mirrored `wire.rs` with it. ADR 0003's launch contract is unchanged in substance — only where its names are defined moves.

Out of scope: `Draining`/checkpoint lifecycle (#42, #43), which follows the same split when implemented.

## Consequences

**Positive**

- The operator never takes a caliband client, TLS stack, or protocol version dependency; only prospero speaks the wire.
- Renamed or removed caliband flags, env names, and wire fields become compile errors in the consumers instead of silent runtime failures.
- Status has explicit per-field ownership; merge-patch null/empty-array workarounds in the operator go away.
- `kubectl get calibantasks` can distinguish live tasks from finished ones (#37) without weakening sandbox isolation.

**Negative**

- Terminal phase depends on prospero running; without it, finished tasks stay `Running` (today's behaviour, no worse).
- Cross-repo sequencing: caliban (contract crate, caliban#656), prospero (`AgentsSettled`, prospero#228), helm-charts (prospero `patch` on `calibantasks/status`, helm-charts#52), and the operator (server-side apply status, #64; phase mapping, #37).
- Consumers upgrade the contract crate in step with caliban releases — explicit coupling that previously existed implicitly.

**Revisit if**

- A second agent-lifecycle observer appears alongside prospero, or prospero stops being part of every deployment.
- caliban gains a Kubernetes-native completion signal that works for interactive tasks.
- Server-side apply field ownership on `status` proves unreliable across the Kubernetes versions we support.
