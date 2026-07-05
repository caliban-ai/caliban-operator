# caliban-operator container image

The operator ships as a multi-arch container image at
**`ghcr.io/caliban-ai/caliban-operator`** (linux/amd64 + linux/arm64).

## What's in it

A two-stage build (`Dockerfile`):

- **builder** — `rust:1.95-bookworm`, `cargo build --release --bin caliban-operator`
  (rustls/ring + kube; no openssl/protoc/git2 native deps).
- **runtime** — `debian:bookworm-slim` + `ca-certificates`, running the
  `caliban-operator` controller binary as a **non-root** user (uid `10001`,
  matching the Helm chart's `securityContext`). The operator writes nothing to
  disk, so it runs fine under a **read-only root filesystem**.

The image contains only the controller (`caliban-operator`), not the `crdgen`
dev tool. Configuration is entirely via environment (`RUST_LOG`, `CALIBAND_IMAGE`,
`CALIBAND_PORT`, `CALIBAN_WORKSPACE_ROOT`, `CALIBAN_WORKSPACE_STORAGE`,
`CALIBAN_GIT_IMAGE`) — see the `caliban-operator` Helm chart.

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

Cut a release by tagging:

```sh
git tag v0.1.0 && git push origin v0.1.0
```
