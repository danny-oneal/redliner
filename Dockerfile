# syntax=docker/dockerfile:1
# Multi-stage build for the `redliner` CLI.
# cargo-chef caches dependency compilation; the runtime is a distroless,
# nonroot image (glibc + CA certs, no shell or package manager).

FROM rust:1.88-slim-bookworm AS chef
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
RUN cargo build --release -p redliner --locked

FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=builder /app/target/release/redliner /usr/local/bin/redliner
ENTRYPOINT ["/usr/local/bin/redliner"]
