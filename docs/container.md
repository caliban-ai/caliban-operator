# caliban-operator container image

The operator ships as a multi-arch container image at
**`ghcr.io/caliban-ai/caliban-operator`** (linux/amd64 + linux/arm64).

## What's in it

A two-stage build (`Dockerfile`):

- **builder** — `rust:1-bookworm`, `cargo build --release --bin caliban-operator`.
  The exact toolchain comes from `rust-toolchain.toml`, which rustup reads from
  the build context, so the version is written in one place only (#90).
  (rustls/ring + kube; no openssl/protoc/git2 native deps).
- **runtime** — `debian:bookworm-slim` + `ca-certificates`, running the
  `caliban-operator` controller binary as a **non-root** user (uid `10001`,
  matching the Helm chart's `securityContext`). The operator writes nothing to
  disk, so it runs fine under a **read-only root filesystem**.

The image contains only the controller (`caliban-operator`), not the `crdgen`
dev tool. `ENTRYPOINT` is the binary and it takes no arguments.

Configuration is **entirely by environment** — there are no flags and no config
file. See [Deploying and configuring](deploying.md#configuration) for the full
set with defaults; the operator refuses to start on an invalid port window,
selector or uid, naming the offending variable. The Helm chart in
[caliban-ai/helm-charts](https://github.com/caliban-ai/helm-charts) wires a
subset of them and leaves the rest on their compiled defaults.

## Build locally

```sh
docker build -t caliban-operator:dev .
```

## Publishing (CI)

`.github/workflows/release-image.yml` mirrors the sibling repos' **native
per-arch** pipeline (no QEMU):

- **Pull requests** touching the build inputs build **both** arches on native
  runners (amd64 on `ubuntu-latest`, arm64 on `ubuntu-24.04-arm`) with **no push**
  — pure validation.
- **`v*` tags** (and `workflow_dispatch`) build each arch, push it **by digest**,
  then a `merge` job assembles the multi-arch manifest list and pushes the
  `{{version}}` and `sha-<sha>` tags via `docker buildx imagetools create`.

Pin a version tag in a deployment rather than tracking a floating one, so an
operator rollout is reproducible.

Cut a release by tagging:

```sh
git tag v0.6.1 && git push origin v0.6.1
```

Afterwards, bump `appVersion` in the `caliban-operator` Helm chart to match, and
copy any changed `deploy/crd/*.yaml` into the `caliban-crds` chart.
