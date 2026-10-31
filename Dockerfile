# syntax=docker/dockerfile:1
# Multi-stage build for the `redliner` CLI, with `redliner-import` alongside
# (docker run --entrypoint redliner-import <image> openapi <spec-url>).
# cargo-chef caches dependency compilation; the runtime is a distroless,
# nonroot image (glibc + CA certs, no shell or package manager).

FROM rust:1.99-slim-bookworm AS chef
RUN cargo install cargo-chef --locked
WORKDIR /app

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
COPY --from=planner /app/recipe.json recipe.json
# Compile dependencies as a cached layer, keyed only on recipe.json.
RUN cargo chef cook --release --recipe-path recipe.json
# Now the real sources; only this layer rebuilds on a code change.
COPY . .
RUN cargo build --release -p redliner -p redliner-import --locked

FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=builder /app/target/release/redliner /usr/local/bin/redliner
COPY --from=builder /app/target/release/redliner-import /usr/local/bin/redliner-import
# Default CWD so a relative report.dir (default "reports") resolves under the
# writable /tmp (emptyDir in the Job) instead of the read-only root.
WORKDIR /tmp
ENTRYPOINT ["/usr/local/bin/redliner"]
