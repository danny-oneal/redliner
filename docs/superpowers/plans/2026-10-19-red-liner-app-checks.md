# Red Liner — App-Layer Checks Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. REQUIRED during coding: the `rust-skills` skill (idiomatic ownership, error handling, enums over downcast).

**Goal:** Reclassify results that pass at the HTTP layer but fail at the application layer, so the SLO gate tells the truth. The canonical case: a GraphQL server returns `200` with an `errors` payload — today Red Liner counts that as success and the run passes. A `checker` sits between the attacker's result stream and the metrics, zeroes the code and records a descriptive error on a failed check, and the failure then flows into the success rate, the SLO gate, and the error summary.

**Architecture:** Port Go's `checks.go` (`checker`, `newChecker`, `apply`, `errsPresent`, `firstGraphQLError`) as the result-reclassification `ResultSink` seam the plumbing plan left between attacker and collector. One engine prerequisite: the GraphQL-errors check needs the response body, which `volley`'s `AttackResult` does not currently carry (`run_hit` reads `resp.bytes()` only to count `bytes_in`, then discards it). Task 1 adds `AttackResult.body` (matching Go's `vegeta.Result.Body`), populated from the bytes already read. Task 2 adds a `checks` module in `red-liner` — `Checker::new(&ChecksConfig)` and `Checker::apply(&mut AttackResult) -> bool` — and calls `checker.apply(&mut r)` before `metrics.add(&r)` in each measured phase of `run`. Nothing else moves: pacing, mix, encoders, SLO, exit codes are untouched.

**Tech Stack:** Rust (edition 2021, MSRV 1.88 per workspace). No new dependencies: `serde_json` (body decode / `errors` walk) and `http` are already in `red-liner`; the body field is plain `Vec<u8>`.

**Spec:** The Go implementation is the behavioral authority — `checks.go` (`checker`, `newChecker`, `apply`, `errsPresent`, `firstGraphQLError`) and `main.go`'s measured-phase loop (`check.apply(res); coll.Add(res, ph.name)`). This is the "App-layer checks (status set + GraphQL `errors`) — a result-reclassification `ResultSink` seam between attacker and collector" item deferred in `docs/superpowers/plans/2026-10-17-red-liner-rust-plumbing.md`.

## Global Constraints

- Config parsing/defaults for `checks` already exist and are **not** re-done: `ChecksConfig { statuses: Vec<i64> (yaml key `status`), graphql_errors: Option<bool> }`. `apply_defaults` already resolves the tri-state — `graphql_errors` is `Some(true)` under the `graphql` protocol and `Some(false)` under `rest` by the time `load_config_file` returns. Treat this as given.
- `volley` gains **no new dependency** — the `body` field is a plain `Vec<u8>`. The engine stays protocol-agnostic: it captures the body it already reads but does not interpret it. All GraphQL/status knowledge lives in `red-liner`'s `checks` module.
- Exit-code contract unchanged: `0` pass, `1` SLO violated, `2` config/runtime error. A failed check does not change exit logic directly — it lowers `success`/`throughput`, which the existing SLO gate reads.
- A transport failure is never masked: if `AttackResult.error` is already set, `apply` leaves the result untouched and returns `false` (Go `apply` first branch).
- Warmup stays unchecked (Go runs `check.apply` only in the measured-phase loop); the warmup drain in `run` is not touched.
- Seeded runs stay reproducible: `apply` is pure reclassification over an already-fired result — no rng, no ordering change.

## Review Focus

Inputs the Go spec implies that a happy-path test never exercises; each is pinned to a task.

- **A transport error is not overwritten by a check** → a result with `error = Some("connection refused")`, `code = 0` passes through `apply` unchanged and returns `false`, so the original cause survives into the error summary. (Task 2)
- **A `200` with a GraphQL `errors` payload trips the SLO gate** → reclassification lowers `success` to ~0, and an enforced `success_rate` SLO then fails the run end-to-end (exit `1`). (Task 2, e2e)
- **Status-set check both ways** → with `status: [200]`, a `500` is reclassified (`code → 0`, descriptive error) and a `200` is left alone. (Task 2)
- **`graphql_errors` off leaves payloads untouched** → the REST default (`Some(false)`) does not reclassify a body that happens to contain an `errors` array. (Task 2)
- **Empty / non-JSON body is not reclassified** → `graphql_errors` on but an empty or unparseable body returns `false` rather than panicking (guards the engine's empty-body results: transport-error and target-error paths carry no body). (Task 1 enables; Task 2 tests)

---

## File Structure

```
crates/
  volley/
    src/target.rs           + pub body: Vec<u8> on AttackResult (Task 1)
    src/attacker.rs          run_hit keeps the response bytes as body (Task 1)
    src/metrics.rs           test literal gains body: vec![] (Task 1, compile)
  red-liner/
    src/checks.rs           NEW: Checker, Checker::new, apply, first_graphql_error (Task 2)
    src/lib.rs              + pub mod checks; (Task 2)
    src/run.rs              build Checker, apply before metrics.add in each phase (Task 2)
    src/slo.rs              test literal gains body: vec![] (Task 2, compile)
    tests/e2e.rs            + e2e_graphql_errors_fail_slo + an all-errors mock (Task 2)
```

---

## Task 1: `AttackResult.body` — capture the response body in the engine

Pure engine prerequisite. Deliverable: every `AttackResult` from a successful transport carries the decoded response body; error/target-failure results carry an empty body; all existing tests stay green.

**Files:**
- Modify: `crates/volley/src/target.rs` (add field), `crates/volley/src/attacker.rs` (`run_hit` keeps bytes), `crates/volley/src/metrics.rs` (test literal)
- Test: inline test in `attacker.rs`
- Reference: Go `vegeta.Result.Body`.

**Interfaces:**
- Produces: `AttackResult` gains `pub body: Vec<u8>`. `run_hit`'s success path sets `body: body.to_vec()` (the bytes it already reads for `bytes_in`); the three failure paths (target error, send error, body-read error) set `body: Vec::new()`.
- Consumes: nothing new.

- [ ] **Step 1: Write the failing test.** In `attacker.rs`, add `spawn_body_server(body: &'static str)` (a clone of `spawn_ok_server` that answers `200` with `Content-Type: application/json`, a matching `Content-Length`, and `body`), and a test `attack_captures_body`: a 50 rps / 300ms attack against it → every result has `r.error.is_none()`, `r.code == 200`, `r.body == body.as_bytes()`, and `r.bytes_in == body.len() as u64`.
- [ ] **Step 2: Run to verify it fails** — `cargo test -p volley attack_captures_body` → FAIL to compile (no `body` field) / assertion.
- [ ] **Step 3: Implement.** Add `pub body: Vec<u8>` to `AttackResult`. In `run_hit`, the `Ok(resp)`→`Ok(body)` arm sets `body: body.to_vec()` (keep `bytes_in: body.len() as u64`); the target-error, send-error, and body-read-error arms set `body: Vec::new()`. Add `body: vec![]` to the `metrics.rs` test literal so the crate compiles.
- [ ] **Step 4: Run to verify it passes** — `cargo test -p volley` → PASS (new test + all prior).
- [ ] **Step 5: Commit** — `feat(volley): capture response body on AttackResult`.

---

## Task 2: `checks` module — reclassify at the app layer, wired into `run`

Ports `checks.go`. Deliverable: a config with `checks.graphql_errors` (default on for GraphQL) or `checks.status` reclassifies failing responses, lowering the success rate so the SLO gate fails the run.

**Files:**
- Create: `crates/red-liner/src/checks.rs`
- Modify: `crates/red-liner/src/lib.rs` (`pub mod checks;`), `crates/red-liner/src/run.rs` (build `Checker`, apply before `metrics.add`), `crates/red-liner/src/slo.rs` (test literal `body: vec![]`)
- Test: inline tests in `checks.rs`; one e2e in `tests/e2e.rs` + an all-errors mock
- Reference: Go `checks.go`, `main.go` measured-phase loop.

**Interfaces:**
- Consumes: `ChecksConfig` (config, existing); `volley::AttackResult` (Task 1); `serde_json::Value`.
- Produces:
  - `src/checks.rs`:
    - `pub struct Checker { statuses: Option<std::collections::HashSet<u16>>, graphql_errors: bool }`.
    - `pub fn new(cfg: &ChecksConfig) -> Checker` — `statuses = None` when `cfg.statuses` is empty, else the set of `cfg.statuses` cast `i64 as u16`; `graphql_errors = cfg.graphql_errors.unwrap_or(false)`. (Go `newChecker`.)
    - `pub fn apply(&self, r: &mut AttackResult) -> bool` — port Go `apply`: (1) if `r.error` is `Some(non-empty)` → return `false` (transport already failed); (2) if `statuses` is set and does not contain `r.code` → `r.error = Some(format!("check: status {} not in allowed set", r.code))`, `r.code = 0`, return `true`; (3) if `graphql_errors` and `(200..400).contains(&r.code)` and `!r.body.is_empty()` → decode JSON, and if `first_graphql_error` is `Some(msg)` → `r.error = Some(format!("check: graphql errors (http {}): {msg}", r.code))`, `r.code = 0`, return `true`; else `false`.
    - `fn first_graphql_error(doc: &Value) -> Option<String>` — `None` unless a non-empty top-level `errors` array; else the first entry's `.message` string, or `"unknown error shape"` (Go `errsPresent` + `firstGraphQLError`). *(A near-identical private helper exists in `data/harvest.rs` with a comment noting no shared home was reachable; this is checks.go's canonical home. Kept separate to stay surgical; unifying the two is a later cleanup, not this plan.)*
  - `src/run.rs`: after building the targeter, `let checker = checks::Checker::new(&cfg.checks);`. In the measured-phase drain loop, change `while let Some(r) = rx.recv().await { metrics.add(&r); }` to bind `mut r`, call `checker.apply(&mut r);`, then `metrics.add(&r);`. Warmup loop unchanged.

- [ ] **Step 1: Write the failing tests.**
  - In `checks.rs` (a helper `res(code, body, err)` building an `AttackResult`):
    - `reclassifies_graphql_errors`: `Checker { graphql_errors: true, statuses: None }`; `res(200, b"{\"errors\":[{\"message\":\"boom\"}]}", None)` → `apply` returns `true`, `r.code == 0`, `r.error` contains `"graphql errors"` and `"boom"`. *(Review Focus)*
    - `graphql_ok_body_passes`: same checker, `res(200, b"{\"data\":{\"x\":1}}", None)` → `false`, `code == 200`, `error.is_none()`.
    - `graphql_errors_off_ignores_payload`: `Checker { graphql_errors: false, statuses: None }`, `res(200, b"{\"errors\":[{\"message\":\"boom\"}]}", None)` → `false`, `code == 200`. *(Review Focus)*
    - `status_set_both_ways`: `Checker { statuses: Some({200}), graphql_errors: false }`; `res(500, b"", None)` → `true`, `code == 0`, error contains `"status 500 not in allowed set"`; `res(200, b"", None)` → `false`, `code == 200`. *(Review Focus)*
    - `transport_error_preserved`: any checker, `res(0, b"", Some("connection refused"))` → `false`, `code == 0`, `error == Some("connection refused")` (unchanged). *(Review Focus)*
    - `empty_body_not_reclassified`: `graphql_errors: true`, `res(200, b"", None)` → `false`, `code == 200`. *(Review Focus)*
  - In `tests/e2e.rs`: add `spawn_error_mock()` (clone `spawn_mock` answering every request with `{"errors":[{"message":"boom"}]}`), and `e2e_graphql_errors_fail_slo`: a GraphQL config at that mock, `constant 50 rps` for `1s`, `slos: { success_rate: 0.99 }` → `run` returns `Exit::SloFailed` (reclassification drops `success` to ~0, tripping the enforced SLO). *(Review Focus: SLO gate)*
- [ ] **Step 2: Run to verify they fail** — `cargo test -p red-liner checks` and `cargo test -p red-liner --test e2e e2e_graphql_errors` → FAIL (module missing / SLO still passes because nothing reclassifies).
- [ ] **Step 3: Implement.** Add `pub mod checks;` to `lib.rs`. Write `checks.rs` porting `checks.go`. Add `body: vec![]` to the `slo.rs` test literal. Wire `Checker` into `run.rs` as above.
- [ ] **Step 4: Run to verify they pass** — `cargo test --workspace` → PASS (checks unit + e2e green; all prior green — existing e2e GraphQL run still passes because its mock returns `{"data":{}}`, which has no `errors`).
- [ ] **Step 5: Commit** — `feat(red-liner): app-layer checks reclassify failed responses before the SLO gate`.

---

## Self-Review notes

- **Spec coverage:** `checks.go` `checker`/`newChecker`/`apply`/`errsPresent`/`firstGraphQLError` → Task 2; the measured-phase `check.apply(res); coll.Add(...)` ordering → Task 2's `run.rs` wiring (collector itself is the separate reports increment, still deferred). `vegeta.Result.Body` → Task 1. Config/defaults already shipped — intentionally not re-done.
- **Why the engine change:** the GraphQL-errors check is impossible without the response body, and `run_hit` discards it today. Capturing it is the minimal, spec-matching enabler (Go keeps `Result.Body`); the body rides the existing per-hit channel and is dropped after check+metrics, so retention is transient.
- **Type consistency:** `Checker`, `Checker::new`, `apply`, `first_graphql_error`, and `AttackResult.body` are named identically across producer (Tasks 1/2) and consumer (`run.rs`). `apply` takes `&mut AttackResult` at both its definition and its `run.rs` call site.
- **Review Focus:** six lines — transport-error preservation, SLO-gate end-to-end, status-set both ways, graphql_errors-off, empty-body guard — each owned by a named test (the body-capture enabler by Task 1's `attack_captures_body`).
- **Proportion:** signatures + test names/assertions with exact values; no Go bodies transcribed — the Go source travels as the spec, cited by function name.
