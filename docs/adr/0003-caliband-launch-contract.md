# ADR 0003 · The caliband launch contract (daemon args + workspace init)

- **Status:** accepted
- **Date:** 2026-07-05
- **Source:** caliban [#366](https://github.com/caliban-ai/caliban/issues/366) (follow-up to [#283](https://github.com/caliban-ai/caliban/issues/283)) · epic [#274](https://github.com/caliban-ai/caliban/issues/274) · builds on [ADR 0002](0002-reconcile-calibantask-to-sandbox.md)

## Context

[ADR 0002](0002-reconcile-calibantask-to-sandbox.md) reconciled a `CalibanTask` into a
correctly-shaped `Sandbox`/SA/NetworkPolicy but explicitly deferred the exact caliband
container contract ("the exact caliband env contract isn't built yet"). A home-cluster
dry run exposed that the deferred contract is load-bearing: **the pod as built by #283
would not run a live caliband.** Verified against caliban's `caliband.rs` arg parser and
the caliban image's Dockerfile:

- The image is `ENTRYPOINT ["caliband"]`, `CMD ["--help"]`. #283 set no container
  `command`/`args`, so the pod runs **`caliband --help` → prints usage → exits →
  CrashLoop**.
- caliband takes `--workspace-root <path>` as a **required flag** (there is no env for
  it); #283 set `CALIBAN_WORKSPACE_ROOT` env, which caliband ignores.
- The network switch is `--listen <host:port>` (or `CALIBAN_DAEMON_LISTEN`, format
  `host:port`); #283 set `CALIBAND_LISTEN=tcp://…` — wrong name and wrong format.
- `CALIBAN_WORKSPACE_SOURCES` (an invented env) is read by nothing, and **no one clones
  the workspace sources into the `/work` PVC**, so the workspace is empty.
- caliband's `--tls-cert/--tls-key/--token` are **optional**, so `--listen` without them
  is valid **plaintext TCP**.

## Decision

1. **Run caliband as a daemon via container `args`.** `build_sandbox` sets the caliband
   container's `args` to `["--workspace-root", <workspace_root>, "--listen",
   "0.0.0.0:<caliband_port>"]`, replacing the image's `--help` CMD (the `caliband`
   ENTRYPOINT is kept). This uses only flags caliband actually parses.

2. **Drop the mismatched env.** Remove `CALIBAND_LISTEN`, `CALIBAN_WORKSPACE_ROOT`, and
   `CALIBAN_WORKSPACE_SOURCES` (none are read; listen + root are now flags, sources are
   handled by the init container). Keep `GONZALO_ENDPOINT` / `CALIBAN_ROUTER_CONFIG_REF`
   (consumed by the caliban agent runtime, harmless if unset).

3. **Populate the workspace with a git-clone init container.** Add an init container
   (image from a new `git_image` setting, default a public `alpine/git`) that mounts the
   workspace volume and, for each `spec.workspace.sources[]`, clones `repo` at `ref` into
   `path` — idempotently (skip if `<path>/.git` already exists, so pause/resume and pod
   restarts over the persistent PVC don't refetch or fail). Public sources only for now;
   private-source auth is a follow-up.

4. **Plaintext TCP for the first e2e.** The operator runs caliband with `--listen` and
   **no TLS/token**. This is sufficient to prove the pipeline (a reachable caliband on the
   Sandbox DNS) and matches caliban's optional-TLS design. Provisioning per-Sandbox certs
   + a bearer token (and wiring prospero's matching client) is a tracked security
   follow-up — the transport already supports it (caliban #280).

## Consequences

- **Applying a `CalibanTask` now yields a caliband that actually starts, has its workspace
  cloned, and listens** on the Sandbox endpoint — closing the gap between "objects render"
  and "an agent can run". This unblocks the home-cluster e2e.
- **Plaintext TCP is a deliberate, temporary posture.** The default-deny NetworkPolicy
  (ADR 0002) still constrains who can reach the port, but the control/stream bytes are
  unencrypted and unauthenticated until the TLS/token follow-up lands. Not for
  multi-tenant/production use.
- **The init container needs a git image and egress** to clone sources — the ADR 0002
  NetworkPolicy already allows general egress. Private sources will need credential
  projection (deferred).
- **`advertise-host` / per-agent ports are not yet set.** Control-plane reachability
  (`serviceFQDN:<port>`) works; per-agent attach dialing from outside the pod (prospero's
  session plane) needs `--advertise-host <serviceFQDN>` + per-agent port routing — a
  follow-up paired with prospero #77.
- **`git clone --depth 1 --branch <ref>` accepts branch/tag names only, not commit
  SHAs** — a `ref` set to a raw commit SHA will fail the clone (the CRD's `ref` defaults
  to `main`; SHA-pinning is a follow-up).
