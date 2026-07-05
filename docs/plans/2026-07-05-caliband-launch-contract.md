# caliband launch contract — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:subagent-driven-development.

**Goal:** Make the reconciled Sandbox actually run a live caliband: daemon `args`, a git-clone init container to populate the workspace, and the mismatched env removed. See [ADR 0003](../adr/0003-caliband-launch-contract.md).

**Scope:** `caliban-operator` only — `src/resources.rs` (`build_sandbox`/`caliband_env`) + `src/config.rs` (`Settings`). Pure builders, unit-tested.

## Global Constraints
- Use only flags caliband's arg parser accepts: `--workspace-root <path>`, `--listen <host:port>` (plaintext OK; TLS/token deferred). `--workspace-root` is **required**.
- Cluster-agnostic: the git image is a `Settings` knob with a neutral public default; no home-cluster identifiers.
- Idempotent clone (skip if `<path>/.git` exists) — the workspace PVC persists across restarts.
- `LocalFleet`/other behavior unaffected; keep the `common_labels` on the podTemplate (NetworkPolicy selector) and the token-less SA.

---

### Task 1: Run caliband as a daemon + drop mismatched env

**Files:** `src/resources.rs` (`build_sandbox`, `caliband_env`).

- [ ] In `build_sandbox`, set the caliband container's `args` (k8s `args` overrides the image `CMD ["--help"]`, keeps `ENTRYPOINT ["caliband"]`):
  ```rust
  args: Some(vec![
      "--workspace-root".to_string(), s.workspace_root.clone(),
      "--listen".to_string(), format!("0.0.0.0:{}", s.caliband_port),
  ]),
  ```
- [ ] In `caliband_env`, REMOVE the three vars caliband/caliban do not read: `CALIBAND_LISTEN`, `CALIBAN_WORKSPACE_ROOT`, `CALIBAN_WORKSPACE_SOURCES`. KEEP the conditional `GONZALO_ENDPOINT` and `CALIBAN_ROUTER_CONFIG_REF` (agent-runtime env, harmless). `caliband_env` may now return an empty vec when neither is set — that's fine (`env: Some(vec![])` or drop `env` when empty; either is acceptable).
- [ ] Update tests: the existing `sandbox_has_caliband_container_pvc_and_service` asserts `CALIBAND_LISTEN`/`CALIBAN_WORKSPACE_ROOT` env — replace those assertions with: the container `args` contain `--workspace-root <root>` and `--listen 0.0.0.0:<port>` (in order), and assert the three removed env vars are ABSENT. Keep the router-config env test (`sandbox_projects_router_config_ref_env_when_model_set`) and the runtimeClass test.
- [ ] `cargo fmt --all`; `cargo clippy --workspace --all-targets --features … -- -D warnings` (match the repo's gate); `cargo test --workspace`. All green. Regenerate CRD if crdgen output changes (it won't — no CRD change).
- [ ] Commit: `fix(operator): run caliband as a daemon (--workspace-root/--listen), drop mismatched env (#366)`

---

### Task 2: git-clone init container to populate the workspace

**Files:** `src/config.rs` (`Settings`), `src/resources.rs` (`build_sandbox`).

- [ ] Add a `git_image: String` field to `Settings` (default a neutral public image, e.g. `"alpine/git:latest"`; env `CALIBAN_GIT_IMAGE`). Add to `Settings::default()` + `from_env()`. Assert the default is neutral (no "home") in the existing `from_env_defaults_are_neutral` test.
- [ ] In `build_sandbox`, add an `init_containers` entry to the `PodSpec` that mounts the workspace volume (same `WORKSPACE_VOLUME` at `s.workspace_root`) and clones each source idempotently. Build the shell script from `t.spec.workspace.sources`:
  ```rust
  fn clone_script(t: &CalibanTask) -> String {
      let mut s = String::from("set -eu\n");
      for src in &t.spec.workspace.sources {
          // idempotent: skip if already cloned onto the persistent PVC
          s.push_str(&format!(
              "if [ ! -d '{path}/.git' ]; then git clone --depth 1 --branch '{r#ref}' '{repo}' '{path}'; fi\n",
              path = src.path, r#ref = src.r#ref, repo = src.repo,
          ));
      }
      s
  }
  ```
  The init container:
  ```rust
  Container {
      name: "clone-workspace".to_string(),
      image: Some(s.git_image.clone()),
      command: Some(vec!["/bin/sh".to_string(), "-c".to_string(), clone_script(t)]),
      volume_mounts: Some(vec![VolumeMount {
          name: WORKSPACE_VOLUME.to_string(),
          mount_path: s.workspace_root.clone(),
          ..Default::default()
      }]),
      ..Default::default()
  }
  ```
  Set `pod_spec.init_containers = Some(vec![that])`. If `sources` is empty, still emit the container with a no-op script (harmless) OR skip it — pick one and note it (skipping when empty is cleaner).
- [ ] Tests: `build_sandbox` for a task with a source produces an init container named `clone-workspace` whose image is the `git_image` default, mounting the workspace volume, whose command script contains `git clone --depth 1 --branch main '<repo>' '<path>'` and the idempotency guard `[ ! -d '<path>/.git' ]`. (Use the existing `task()` fixture's source.)
- [ ] Gate green (fmt/clippy/build/test). CRD unchanged.
- [ ] Commit: `feat(operator): git-clone init container populates the workspace PVC (#366)`

---

## Notes
- **Deferred (ADR 0003):** TLS/token for the transport (plaintext for now); `--advertise-host`/per-agent port routing (pairs with prospero #77); private-source credential projection.
- The caliband arg parser is in `caliban/crates/caliban-supervisor/src/bin/caliban.rs` (`caliband.rs`) — flags: `--workspace-root` (required), `--listen`, `--socket-path`, `--data-base`, `--advertise-host`, `--agent-port-base`, `--tls-*`, `--token`.
