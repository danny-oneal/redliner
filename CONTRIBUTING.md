# Contributing to Redliner

Thanks for helping improve Redliner. This guide covers the local workflow and
the conventions the codebase already follows.

## Prerequisites

- A Rust toolchain **≥ 1.88** (`rustup`); the pinned channel is in
  [`rust-toolchain.toml`](rust-toolchain.toml).
- No other services are required — the test suite spins up its own in-process
  mock servers.

## Workspace layout

```
Cargo.toml              workspace manifest
crates/
  volley/               async load engine, package redliner-volley (no dependency on redliner)
  redliner/            the binary: config, encoders, pools, checks, SLOs, reports
configs/                example scenario files
docs/                   configuration reference and implementation plans
```

`volley` is deliberately protocol- and config-agnostic: it takes plain Rust
types so it stays reusable and unit-testable. Keep config/CLI/protocol concerns
in `redliner`.

## Everyday commands

```bash
make test     # cargo test --workspace
make lint     # cargo clippy --workspace --all-targets -- -D warnings
make build    # release binary
make run CONFIG=configs/rest-example.yaml
make validate CONFIG=configs/rest-example.yaml
```

CI runs `make test` and `make lint`; both must pass. Keep clippy clean —
warnings are errors.

## Testing

The project is test-driven: write the failing test first, watch it fail, then
make it pass. Tests live beside the code they cover (`#[cfg(test)]` modules),
with end-to-end coverage in `crates/redliner/tests/` against hand-rolled mock
servers. Seeded runs are reproducible, so assert on concrete values rather than
ranges where you can.

When you touch behavior, add or adjust a test in the same change; a bug fix
starts with a test that reproduces the bug.

## Code style

- The code is **hand-formatted** to a tight style; the project does **not** run
  `cargo fmt` across the tree, and CI does not gate on it. Match the
  surrounding style rather than reformatting existing code.
- Prefer small, surgical diffs. Don't refactor adjacent code that isn't part of
  your change.
- Favor idiomatic Rust: typed errors with `thiserror` in libraries, `anyhow`
  mapped to exit codes in the binary; enums over downcasting; ownership over
  cloning where it reads cleanly.

## Commits and pull requests

- Use [Conventional Commits](https://www.conventionalcommits.org):
  `feat:`, `fix:`, `chore:`, `refactor:`, `test:`, `docs:`, with an optional
  scope — `feat(redliner): ...`, `feat(volley): ...`, `feat(config): ...`.
- Keep each commit coherent and green (`make test && make lint`).
- In the PR description, say what changed and why, and note the test coverage.
- Update [`CHANGELOG.md`](CHANGELOG.md) under *Unreleased* for user-visible
  changes.

## Releasing

Versions follow [SemVer](https://semver.org). Both crates share one version,
set once under `[workspace.package]` in the root `Cargo.toml`, and redliner
pins the engine to exactly that version. Don't tag by hand. To cut a release:

1. On a branch from `main`, run `make release-patch`, `make release-minor`
   or `make release-major` to bump that part of the current version, or
   `make release VERSION=X.Y.Z` to set one. It bumps the version and the
   engine pin, moves the changelog's *Unreleased* section
   under `## [X.Y.Z] - <today>`, and refreshes `Cargo.lock`.
2. Commit that as `chore(release): X.Y.Z` and open a PR. The PR builds every
   target, and the build fails if the changelog has no notes for `X.Y.Z`.
3. Merge it. `.github/workflows/release.yml` sees a version with no `vX.Y.Z`
   tag, builds and smoke-tests every target (the binary must report `X.Y.Z`),
   then publishes the GitHub release. That creates the tag, uses the
   changelog section as the notes, and attaches each archive with the
   `.sha256` checksum that `scripts/install.sh` requires. 0.x versions are
   published as pre-releases.

Pushes to `main` that don't change the version build nothing. A hand-pushed
tag that doesn't match the crate version fails the workflow. A PR that touches
the workflow, the release scripts or the workspace `Cargo.toml` runs the same
builds without publishing. `make test-install` and `make test-release` test
the scripts locally.

Both crates go to crates.io, engine first: `cargo publish -p redliner-volley`,
then `cargo publish -p redliner`. The engine's package name is
`redliner-volley` because `volley` belongs to an unrelated crate; redliner
renames the dependency back to `volley`. CI runs `cargo package --workspace`,
which builds the packaged `redliner` against the packaged engine.
