# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

Redliner is an SLO-driven HTTP load tester (a Rust port of an internal Go/Vegeta harness). User-facing docs: `README.md`, config schema: `docs/configuration.md`.

## Commands

```bash
make test                          # cargo test --workspace
make lint                          # cargo clippy --workspace --all-targets -- -D warnings
make validate CONFIG=configs/rest-example.yaml   # parse + validate a config, fire nothing
make run CONFIG=configs/rest-example.yaml

cargo test -p redliner-volley                      # one crate
cargo test -p redliner --test e2e                 # one integration test file
cargo test -p redliner --lib replay::schedule     # unit tests under a module path
cargo test -p redliner --test e2e <name> -- --exact --nocapture
```

CI runs `make lint`, `make test`, `cargo +1.88 check` (MSRV), `cargo package --workspace` (both crates publishable) `make test-install` (shellcheck + `scripts/install.sh` against a fake release) and `make test-release` (the release scripts); all must pass. Clippy warnings are errors. MSRV is 1.88, edition 2021; bump `rust-version` and the CI msrv job together.

**Do not run `cargo fmt`.** The tree is hand-formatted; CI doesn't gate on fmt, and a tree-wide reformat produces an unreviewable diff. Match the surrounding style.

## Architecture

Two workspace crates with a one-way dependency: `redliner` → `volley`.

- **`volley`** — protocol- and config-agnostic engine. `Attacker::attack(targeter, pacer, duration, phase)` returns an mpsc receiver of `AttackResult`s; in-flight requests are capped by a semaphore at `max_workers`. The two seams are the `Pacer` trait (`pace(elapsed, hits) -> (wait, stop)`) and the `Targeter` trait (`target(seq, &mut Target)`). Never add config, CLI, or protocol knowledge here.
- **`redliner`** — `main.rs` is a shim; everything is in the lib. The `compare` subcommand (`compare.rs`, dispatched at the top of `run_with_interrupt`) only reads two finished report dirs; it never fires traffic. `run.rs::run_inner` is the pipeline and the best entry point for understanding the flow:
  1. `config::load_config_file_with` — decode YAML, apply CLI overrides (`cli.rs`), resolve relative file paths against the **config file's directory**, then validate. With no config file, `--url` alone builds a one-scenario REST config in memory (`Args::url_request`) and goes through the same defaults and validation (`config::load_config_with`).
  2. Replay (if `cfg.replay` is set) parses, matches, and schedules the whole log *before* anything else, so a bad log fails early.
  3. `encode::new_encoder` picks GraphQL or REST behind the `Encoder` trait; `data::build_pools` + `data::harvest::run_harvest` fill pools (harvest probes the live service first).
  4. Plan is either `Profile` (`MixTargeter` + weighted `mix.rs` + `phases.rs` pacers, with an unmeasured warmup at 10% rate) or `Replay` (`ScheduleTargeter` + `SchedulePacer` from `replay/`).
  5. A report dir and initial `run_manifest.json` are written **before traffic**, so aborted runs leave a trail.
  6. Each result goes through `checks::Checker` (reclassifies app-layer failures, e.g. a 200 with GraphQL `errors`) into `report::Collector`.
  7. `slo::evaluate` → reports/JUnit → `Exit` code: `0` ok, `1` SLO failed (or replay dropped hits — the synthetic `replay_delivery` SLO), `2` config/runtime error, `130` interrupted by Ctrl-C (`run_with_interrupt` stops firing, drains in-flight hits, and reports the partial run with `aborted: true`).

Invariants worth knowing before you change things:

- **Every config struct is `#[serde(deny_unknown_fields, default)]`.** A new field needs a sensible `Default`, validation in `config/mod.rs`, a test in `config/tests.rs`, and a row in `docs/configuration.md`.
- **Reproducibility:** all randomness flows from `load.seed` through `ChaCha8Rng` (`seed: 0` means time-based). Don't introduce unseeded RNGs; tests assert concrete values from fixed seeds.
- **Templating:** a value that is *exactly* `{{pool}}` keeps its native type; an embedded placeholder interpolates as a string (`data::render_value` / `render_string`).
- **Replay vs profile:** `weight` is ignored in replay mode and `match` in profile mode, so one config serves both. Overlapping `match` rules are an error (`ReplayError::Ambiguous`).
- Auth credentials are read from env vars named in config (`token_env` etc.), never stored in the file.

## Testing

Test-driven: write the failing test first; a bug fix starts with a test that reproduces it. Unit tests live in `#[cfg(test)]` modules beside the code (`config/` keeps them in `config/tests.rs`). End-to-end tests in `crates/redliner/tests/e2e.rs` drive `redliner::run(Args)` against hand-rolled in-process TCP mock servers (`spawn_mock`, `spawn_mock_seeded`); no external services are needed. Fixtures are in `crates/redliner/tests/fixtures/`.

## Rust conventions

Use the `rust-skills` skill when writing or reviewing Rust here. Where it conflicts with this file, this file wins (notably: no `cargo fmt`, edition-2021 not 2024 syntax). The rules this codebase already follows:

- Errors: a `thiserror` enum per module (`EncodeError`, `ReplayError`, `ConfigError`, `volley::Error`); `anyhow` only in `run.rs`, mapped to `Exit`. Lowercase messages, `#[source]` on wrapped errors.
- No `unwrap()` outside tests; a poisoned lock maps to an error rather than panicking (see `MixTargeter::target`).
- `BTreeMap` for anything that reaches reports or output, so ordering is deterministic.
- Small, surgical diffs; don't refactor adjacent code. Prefer enums over downcasting, borrowing over cloning.

## Workflow

- Conventional Commits with an optional crate/area scope: `feat(volley): ...`, `fix(config): ...`.
- Add user-visible changes to `CHANGELOG.md` under *Unreleased*.
- Never tag releases by hand. The version lives once, in `[workspace.package]` of the root `Cargo.toml`. A release is a `chore(release): X.Y.Z` PR made with `make release-patch` / `release-minor` / `release-major` (or `make release VERSION=X.Y.Z`); merging it makes `release.yml` tag `vX.Y.Z`, build every target and publish the GitHub release, with that version's CHANGELOG section as the notes.
- Larger features get a design spec in `docs/superpowers/specs/` and an implementation plan in `docs/superpowers/plans/` (dated `YYYY-MM-DD-<topic>.md`); read the relevant ones before extending replay or the Docker/K3s deploy.
- `codebook.toml` is the spell-checker word list; add project terms there.
