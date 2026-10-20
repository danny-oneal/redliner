# Changelog

All notable changes to Red Liner are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Removed
- The Go/Vegeta reference implementation. Red Liner is now a pure Rust
  workspace; the Go sources served only as the behavioral spec during the port.

## [0.1.0]

Initial Rust release — a from-scratch port of the internal Go/Vegeta harness.

### Added
- **`volley` engine** — protocol-agnostic async load generation: `Target` /
  `AttackResult` / `Metrics`, the `Pacer` and `Targeter` traits, constant /
  Poisson (seeded, self-correcting) / linear pacers, and a tokio `Attacker`
  that fires at a paced rate over reqwest.
- **Declarative YAML config** with defaults, fail-fast validation (unknown keys
  rejected), Go-style durations, and config-relative file resolution.
- **GraphQL and REST** encoders behind one seam, selected by `target.protocol`,
  with env-sourced auth (`none` / `bearer` / `basic` / `header`) and a weighted
  operation mix.
- **Load profiles** `constant`, `poisson`, `ramp`, and `steps`, with an
  unmeasured warmup phase.
- **Data pools and templating** — `{{pool}}` placeholders in paths, bodies, and
  GraphQL variables, drawn from `list` / `range` / `file` pools, preserving
  native scalar types.
- **Harvest** — a pre-attack probe that seeds pools from the live service by
  JSON-path extraction, so every request resolves to real data.
- **App-layer checks** — reclassify a `200`-with-GraphQL-`errors` or an
  out-of-set status as a failure so the success rate and SLO gate tell the
  truth; transport errors are never masked.
- **SLO gate** on success rate, p50/p95/p99, and throughput, driving the exit
  code (`0` pass, `1` SLO violated, `2` config/runtime error).
- **Per-run report directory** — `reports/<timestamp>/` (plus a `latest`
  symlink) with a text summary, latency histogram, `metrics.json`, per-operation
  and per-phase breakdowns, `run_manifest.json`, `junit.xml`, and a raw
  `results.jsonl` stream.
- **Reproducibility** — a fixed `load.seed` reproduces pacing and sampling.
