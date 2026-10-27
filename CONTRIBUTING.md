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
  volley/               async load engine (no dependency on redliner)
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

Versions follow [SemVer](https://semver.org). To cut a release, bump the crate
versions, move the *Unreleased* changelog section under the new version with a
date, and tag `vX.Y.Z`. The tag triggers `.github/workflows/release.yml`,
which builds every target, smoke-tests it, and attaches each archive with a
`.sha256` checksum (which `scripts/install.sh` requires). A PR that touches the
workflow, the install script or the workspace `Cargo.toml` runs the same builds
without publishing; `make test-install` tests the script locally.
