# Changelog

All notable changes to caliban-operator are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).
While the project is pre-1.0, the minor version is bumped for new features and
the patch version for fixes.

The operator is not published to crates.io (`publish = false`). A release is a
container image: tagging a merge commit `v*` fires `release-image.yml`, which
publishes `ghcr.io/caliban-ai/caliban-operator:<version>` for linux/amd64 and
linux/arm64.

Entries before v0.6.0 were back-filled from the `release:` commit messages that
were, until this file existed, the only record of what each tag contained. They
are therefore summaries at the granularity those commits recorded, not the
per-change detail used from v0.6.0 onwards.

## [Unreleased]

### Added

- **Leader election, on by default.** The controller acquires a `coordination.k8s.io`
  Lease before running either reconciler and returns to standby if it loses it, so
  running more than one replica is safe rather than a race between two writers.
  Configured by `CALIBAN_LEADER_ELECTION` (default on), `CALIBAN_LEASE_NAME`,
  `CALIBAN_LEASE_NAMESPACE`, `CALIBAN_LEASE_DURATION_SECONDS` and
  `CALIBAN_LEASE_GRACE_SECONDS`; the holder identity comes from `POD_NAME`.
  Needs the Lease RBAC added in caliban-charts (#48, PR #96)

- **Typed agent permission posture on `Workspace`.** `spec.agentPolicy` gained
  `permissionMode`, `autoAllow` and `noPermissions`, projected into the agent's
  environment instead of travelling as untyped `env` entries. The settings that
  leave an agent unsupervised — `permissionMode: dontAsk` or `bypassPermissions`,
  `noPermissions` — are admitted only on a Workspace whose
  `agentPolicy.allowUnattended` is true, which extends ADR 0006's existing
  authorization rule to the Workspace's own policy (recorded as decision 7 in that
  ADR) (#51, PR #88)

- **Typed agent extension posture on `Workspace`.** `spec.agentPolicy` also gained
  `noMcp`, `noHooks`, `noSkills`, `noSubAgent`, `strictKnownMarketplaces`,
  `enabledPlugins`, `blockedMarketplaces` and `parallelToolLimit`. Typed fields take
  precedence over any same-named `env` entry, and list fields project even when
  empty — an empty `enabledPlugins` means *enable none*, which is a different
  instruction from leaving it unset (#87, PR #89)

- **Per-task model override.** `CalibanTask.spec.model.name` overrides the resolved
  workspace provider's model for one task, applied at the resolve-and-pin step so
  the override is recorded in `status.resolvedWorkspace` like every other pinned
  value (#52, PR #91)

### Fixed

- **The sandbox pod runs as the agent's uid.** The pod spec now carries a
  `securityContext` with `runAsUser`/`runAsGroup`/`fsGroup` set to the agent uid
  (default 10001, configurable via `CALIBAN_AGENT_UID` / `CALIBAN_AGENT_GID`) and
  `runAsNonRoot`. Before this, clones written by the agent were owned by a
  different uid than the one that had to use them (#79, PR #85)

- **An invalid `state.mode` is refused at admission.** The CRD schema now carries
  `enum: [remote, local]`, so the apiserver rejects a bad mode on write — the same
  moment `permissionPosture` already failed. The enum is in the schema and
  deliberately not in the Rust type: a task stored before the enum existed may hold
  any value, and a typed enum would make such an object fail to deserialize
  entirely, which is worse than the clear `Failed` status it gets now. The
  reconcile-time cross-field checks (`remote` needs an endpoint, `local` must not
  have one) are unchanged (#94, PR #97)

### Internal

- The Rust toolchain now lives in exactly one place. `rust-toolchain.toml` moved to
  1.99.0 and is the only pin: `ci.yml` installs with
  `dtolnay/rust-toolchain@stable` and the `Dockerfile` uses `FROM rust:1-bookworm`,
  both deferring to the file, which rustup honours when cargo runs. Previously the
  version was written in three places that could drift (#90, PR #98)

- `caliban-contract` pinned to `=0.15.0`, in lockstep with prospero
  (caliban-ai/prospero#266) so the contract's two downstream consumers never
  disagree about its version. Nothing the operator compiles against changed —
  `launch.rs` is the only file differing between the versions. Worth knowing
  downstream: `CalibandLaunch.router_config` kept its type but changed meaning,
  from inline config JSON to a filesystem path, so a caller passing JSON still
  compiles and silently has it read as a path. The operator passes the mounted
  ConfigMap path (#95, PR #99)

- Documentation audit against the shipped code: CRD and deploying references
  brought current, README moved to v0.6.0, and the missing ADR template restored
  (PR #86)

## [0.6.0] - 2026-09-19

Per-session permission posture, and caliband's launch surface sourced from the
contract crate rather than hand-written strings.

### Added

- `CalibanTask.spec.task.permissionPosture` (`supervised` | `unattended`),
  admitted only under a `Workspace` whose `agentPolicy.allowUnattended` is true.
  The effective posture is reported in `status` and shown as a `Posture` printer
  column. Recorded as ADR 0006 (#81)

### Changed

- caliband's flags and environment variable names now come from
  `caliban-contract` 0.14.0, and provider credential environment names from
  `ProviderKind`, so the operator no longer carries its own copy of caliband's
  launch surface (#83)

- CRD documentation fixes: `tokenRef` principal guidance, and an egress example
  using an RFC 5737 documentation address (#77)

```text
Requires caliban-crds >= 0.2.5 for the permissionPosture / agentPolicy fields.
```

## [0.5.0] - 2026-09-16

Terminal phases, workspace egress control, storage settings and Kubernetes
Events — 17 PRs.

### Added

- `AgentsSettled` resolves to a terminal phase (#75)
- Workspace egress allow-list (#74)
- `spec.state` storage settings plus a gonzalod token (#76)
- Kubernetes Events for reconcile outcomes (#73)
- Sandbox resource requests and limits (#62)
- Configurable cluster DNS domain (#71)
- Server-side-apply status writes (#67)
- A watch on owned `Sandbox` objects (#68)

### Fixed

- Reconcile, config, RBAC and CRD fixes (#60, #61, #63, #65, #69, #70, #72)
- Documentation corrections (#66, #77)

## [0.4.0] - 2026-09-13

Interactive tasks, sandbox reachability fixes, and the removal of Ollama.

### Added

- `CalibanTask.spec.task.interactive` (#28, PR #29)

### Fixed

- Project the provider base URL and API key under provider-native environment
  variable names (#31)
- Pass the session-plane CA to caliband (#33)
- Make `Workspace.spec.sources` optional (#34)
- Pass `--tls-server-name` so workers verify against the certificate SAN (#36)

### Removed

- Ollama support. Samples, CRDs and tests migrated to `kind: openai` with an
  explicit `baseUrl`, which covers the same deployments without a dedicated
  provider kind (#39)

## [0.3.1] - 2026-07-19

### Fixed

- Per-agent stream reachability in Kubernetes. `build_sandbox` passes
  `--advertise-host <sandbox>.<ns>.svc.cluster.local` and pins
  `--agent-port-base`, and the sandbox `NetworkPolicy` opens the per-agent stream
  window (7100–7999) alongside 8443. Without this, prosperod could not attach to
  a k8s agent's output because caliband advertised `tcp://0.0.0.0:7100`
  (#24, #25, PR #26)

## [0.3.0] - 2026-07-18

### Fixed

- `build_sandbox` launches caliband with the shared session-plane bearer token
  and TLS, so the pod binds instead of crash-looping on *"a non-empty bearer
  token is required"* (#22, PR #23)

## [0.2.0] - 2026-07-12

The configuration plane: a `Workspace` custom resource, and resolve-and-pin.

### Added

- `Workspace` CRD (v1alpha1), with `CalibanTask.workspaceRef` / `providerRef`
  resolve-and-pin (#11, PR #12)

### Changed

- **Breaking (pre-v1).** Inline `CalibanTask.spec.workspace` removed in favour of
  `workspaceRef` (#11, PR #12)
- Gate the `CalibanTask` pin on `Workspace` readiness (#13, PR #18)

### Fixed

- CRD schema validation, graceful shutdown, and `serde_yaml` → `serde_norway`
  (#2, PR #10)

### Removed

- Dead `WorkspacePhase::Reconciling` variant (#14, PR #17)
- Orphaned `CalibanTaskStatus.workspace` field (#15, PR #16)

## [0.1.1] - 2026-07-05

First release that reconciles end to end.

### Fixed

- `volumeClaimTemplates` TypeMeta, so a `CalibanTask` reconciles into a `Sandbox`
  that agent-sandbox v0.5.0 accepts (#7, PR #8)

## [0.1.0] - 2026-07-05

### Added

- The container image and its multi-arch release workflow. A two-stage
  `Dockerfile` (`rust:1.95-bookworm` → `debian:bookworm-slim` + ca-certificates)
  builds the controller and runs it non-root as uid 10001, matching the Helm
  chart's `securityContext`; the controller writes nothing to disk, so it runs
  under a read-only root filesystem. `release-image.yml` builds both arches and
  publishes `ghcr.io/caliban-ai/caliban-operator` on `v*` tags
  (caliban-ai/caliban#358, PR #5)

[Unreleased]: https://github.com/caliban-ai/caliban-operator/compare/v0.6.0...HEAD
[0.6.0]: https://github.com/caliban-ai/caliban-operator/compare/v0.5.0...v0.6.0
[0.5.0]: https://github.com/caliban-ai/caliban-operator/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/caliban-ai/caliban-operator/compare/v0.3.1...v0.4.0
[0.3.1]: https://github.com/caliban-ai/caliban-operator/compare/v0.3.0...v0.3.1
[0.3.0]: https://github.com/caliban-ai/caliban-operator/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/caliban-ai/caliban-operator/compare/v0.1.1...v0.2.0
[0.1.1]: https://github.com/caliban-ai/caliban-operator/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/caliban-ai/caliban-operator/releases/tag/v0.1.0
