# Changelog

All notable changes to Redliner are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- **Production-log replay** — replay a structured production log (JSON lines
  or HAR) instead of a synthetic profile. Entries are matched to scenarios by
  per-scenario `match` rules (GraphQL `operation`; REST `method` + a `*` / `**`
  path glob) and fired in the log's order and at its inter-arrival pace through
  the existing attacker, checks, SLOs, and reports. Switch on with a `replay`
  block or `--replay <log>`; `replay.speed` / `--speed` compress time,
  `replay.copies` / `--copies` multiply volume over the same duration,
  `replay.window` slices the log, and `replay.on_unmatched: fail` / `--strict`
  aborts on unmatched entries. Request values still come from data pools.
  Malformed log lines are skipped and counted, a replay summary is printed and
  stored in `run_manifest.json`, and every parse/match failure is exit `2`
  before any request is fired. Epoch timestamp units (s/ms/µs/ns) are inferred
  by magnitude; the post-`speed` span is capped at 24h and the schedule at
  10,000,000 hits (both exit `2`); and a replay that falls behind records
  `fired`/`dropped`, fails a `replay_delivery` check (also in `junit.xml`),
  warns, and exits `1`. See `configs/replay-example.yaml`.
- **Live stats** — a progress line on stderr every 10s during the attack: the
  running request total plus the rate, success, p95 and failures of the last
  window. Set the interval with `report.live_stats` or `--live-stats` (`0s`
  turns it off; anything else must be at least `100ms`).
- **Graceful Ctrl-C** — the first Ctrl-C stops firing, lets in-flight requests
  finish, and writes the full report directory from the partial results
  (`"aborted": true` in `run_manifest.json`, an `INTERRUPTED` line in the
  summary), then exits `130`. A second Ctrl-C quits immediately without
  reports. Library users get `redliner::run_with_interrupt` and
  `volley::Attacker::attack_until`, which stops on a `watch` flag even mid-wait.

- **Static Linux binaries and an install script** — releases now include
  static musl builds for x86_64 and arm64 Linux, and every release asset has a
  `.sha256` checksum. `scripts/install.sh` picks the right asset, verifies the
  checksum (and refuses a release without one), and installs to
  `~/.local/bin`.

### Changed
- Release builds use fat LTO, one codegen unit and stripped symbols, and the
  binary uses mimalloc. At 5000 rps against a local server the load
  generator's own CPU per request is ~28% lower, and the Linux binary shrinks
  from 10.9 MB to ~6 MB.
- `volley::Targeter::target` now takes the hit index `seq: u64` (breaking for
  external `Targeter` implementations).
- CLI overrides are now applied before defaults and validation, so overridden
  values are validated, `--seed 0` derives a wall-clock seed, and `--rate 0` /
  `--duration 0s` fall back to defaults.

### Fixed
- Templating no longer re-parses sampled values: a pool value containing `{{`
  (e.g. harvested or file-pool data) is inserted verbatim instead of failing
  the request as an unknown pool or bad template, and a value that references
  its own pool no longer hangs the run.

## [0.1.0] - 2026-10-20

Initial release — a from-scratch Rust port of an internal Go/Vegeta harness.

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
- **Redline banner** — a tachometer bar printed in the run summary when a run
  violates one or more SLOs.
- **Reproducibility** — a fixed `load.seed` reproduces pacing and sampling.

[Unreleased]: https://github.com/danny-oneal/redliner/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/danny-oneal/redliner/releases/tag/v0.1.0
