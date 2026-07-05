# syntax=docker/dockerfile:1

# ---- builder ----
FROM rust:1.95-bookworm AS builder
WORKDIR /src
# rustls (ring) + kube; no openssl/protoc/git2 native deps, so the base image's
# toolchain is sufficient — no extra apt.
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    cargo build --release --bin caliban-operator

# ---- runtime ----
FROM debian:bookworm-slim AS runtime
# ca-certificates so the kube client can verify TLS (and openssl-probe can find
# the system trust store). The operator writes nothing to disk — it is a
# controller — so it runs happily under a read-only root filesystem.
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/*
# Non-root user matching the chart's securityContext (runAsUser 10001).
RUN useradd --uid 10001 --create-home --home-dir /home/app --shell /usr/sbin/nologin app
COPY --from=builder /src/target/release/caliban-operator /usr/local/bin/caliban-operator
USER 10001
WORKDIR /home/app
# Runs the controller (watch CalibanTasks + reconcile). Config is via env
# (RUST_LOG, CALIBAND_IMAGE, CALIBAND_PORT, CALIBAN_WORKSPACE_*) — see the chart.
ENTRYPOINT ["caliban-operator"]
