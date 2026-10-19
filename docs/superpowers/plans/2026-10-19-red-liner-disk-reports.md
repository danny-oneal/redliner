# Red Liner — Disk Reports Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. REQUIRED during coding: the `rust-skills` skill (idiomatic ownership, error handling, enums over downcast).

**Goal:** Write a per-run report directory to disk so a run leaves an audit trail and CI-ingestible artifacts, not just a terminal summary. Each run creates `reports/<UTC timestamp>/` (plus a refreshed `latest` symlink) holding a text summary, a latency histogram, overall `metrics.json`, per-operation and per-phase summaries, a `run_manifest.json` audit record, `junit.xml` (when `report.junit`), and a raw `results.jsonl` stream (when `report.keep_raw`).

**Architecture:** Port Go's `report.go` (`collector`, `newCollector`, `Add`, `Close`, `newHistogram`, `makeReportDir`, `writeReports`, `opSummary`, `summarize`, `runManifest`) and `slo.go`'s `writeJUnit`. A new `report` module in `red-liner` owns a `Collector` that fans each result into overall/per-operation/per-phase `volley::Metrics` plus a fixed-bucket latency histogram, and optionally appends each record to a raw NDJSON sink. Per-operation attribution reuses the existing `Encoder::operation(method, url)` seam, surfaced through a new `MixTargeter::operation` delegating method so `run` (which already holds `Arc<MixTargeter>`) can attribute each result without the collector owning the encoder. `run` replaces its single `Metrics` with the `Collector`, writes the manifest before the attack (so an aborted run still leaves a trail) and finalizes it after, then writes the report files and gates the exit code exactly as today.

**Tech Stack:** Rust (edition 2021, MSRV 1.88 per workspace). No new dependencies: `serde`/`serde_json` (JSON artifacts) and `serde_yaml_ng` are already in `red-liner`; `quick-xml` is **not** pulled in — JUnit is a tiny hand-written XML string (one `<testsuite>` with `<testcase>`/`<failure>` children), matching Go's fixed schema. `volley` gains nothing: `metrics.json` is built in `red-liner` from `Metrics`' public fields, so `volley` stays serde-free.

**Spec:** The Go implementation is the behavioral authority — `report.go` (`collector`/`Add`/`Close`/`newHistogram`/`makeReportDir`/`writeReports`/`summarize`/`runManifest`) and `slo.go` (`writeJUnit`, `junitSuite`/`junitCase`/`junitFailure`). This is the "Disk reports: per-operation, per-phase, histogram, `metrics.json`, `results.bin` raw stream, `run_manifest.json`, JUnit XML, `reports/<timestamp>/` + `latest` symlink" item deferred in `docs/superpowers/plans/2026-10-17-red-liner-rust-plumbing.md`.

## Global Constraints

- Config parsing/defaults for `report` already exist and are **not** re-done: `ReportConfig { dir, junit, keep_raw: Option<bool>, per_operation: Option<bool> }`; `apply_defaults` sets `dir = "reports"`, `keep_raw = Some(true)`, `per_operation = Some(true)`. The `--out` CLI flag already overrides `report.dir`. Treat these as given.
- `volley` gains **no new dependency** and no serde. `metrics.json` is a `red-liner`-side serializable view built from `Metrics`' public fields.
- Exit-code contract unchanged: `0` pass, `1` SLO violated, `2` config/runtime error. A report-write failure (can't create the dir / a file) is a runtime error → exit `2`, mapped through the existing `anyhow` path. The `latest` symlink is best-effort (Go ignores its error): a symlink failure never fails the run.
- `--validate` and config/load failures return before the attack and write **no** report dir (matching Go).
- **Divergence from Go, confirmed with the user:** the raw stream is `results.jsonl` (newline-delimited JSON, one record per request), not vegeta's `results.bin` gob stream — the Rust port has no vegeta producer/consumer. Gated by `report.keep_raw`.
- Per-operation attribution uses `Encoder::operation` (already implemented for both protocols); a result whose URL matches no operation is counted only in overall and per-phase (Go: `if op := c.opOf(res); op != ""`).

## Review Focus

Inputs the Go spec implies that a happy-path test never exercises; each is pinned to a task.

- **Steps profile → more than one phase → `per_phase.json` is written; a single-phase run omits it** (Go `if len(c.perPhase) > 1`). (Task 1)
- **A result that matches no operation** is still counted in overall/per-phase and does not create a `""`-named per-op bucket. (Task 1)
- **`keep_raw` off → no `results.jsonl`; on → one JSON line per recorded result**, and the file is flushed before the summary is read. (Task 1)
- **`report.junit` off → no `junit.xml`; on → one `<testcase>` per SLO with a `<failure>` only on failed ones**, and `failures`/`tests` counts match. (Task 1)
- **`latest` symlink points at the newest run dir and a symlink failure does not fail the run** (best-effort). (Task 1)
- **Every run now writes a report dir** — existing e2e tests must direct `report.dir` at a temp path so they neither pollute nor depend on the repo's `reports` symlink. (Task 2)

---

## File Structure

```
crates/red-liner/
  src/report.rs          NEW: Collector, LatencyHistogram, OpSummary/summarize,
                              RunManifest, make_report_dir, write_reports, write_junit (Task 1)
  src/lib.rs             + pub mod report; (Task 1)
  src/slo.rs             SloResult derives Serialize (Task 1)
  src/encode/mod.rs      + MixTargeter::operation delegating to the encoder (Task 2)
  src/run.rs             Collector replaces the single Metrics; manifest before/after;
                              write report dir; per-op summary line (Task 2)
  tests/e2e.rs           existing runs pass --out <tempdir>; + e2e_writes_report_dir (Task 2)
```

---

## Task 1: `report` module — collector, histogram, writers, manifest, report dir

Ports `report.go` + `slo.go`'s `writeJUnit`. Pure module, unit-tested against synthetic results and a temp dir; no `run` wiring yet.

**Files:**
- Create: `crates/red-liner/src/report.rs`
- Modify: `crates/red-liner/src/lib.rs` (`pub mod report;`), `crates/red-liner/src/slo.rs` (`#[derive(Serialize)]` on `SloResult`)
- Test: inline tests in `report.rs`
- Reference: Go `report.go`, `slo.go` `writeJUnit`.

**Interfaces:**
- Consumes: `volley::{Metrics, AttackResult}`; `crate::config::Config`; `crate::slo::SloResult`; `serde`, `serde_json`.
- Produces:
  - `src/report.rs`:
    - `pub struct LatencyHistogram { buckets: Vec<Duration>, counts: Vec<u64>, total: u64 }` with `new()` (Go `newHistogram` bucket edges: 0, 500µs, 1ms, 2, 5, 10, 25, 50, 100, 250, 500ms, 1s, 2s, 5s), `add(&mut self, latency: Duration)` (increment the highest bucket whose edge `<= latency`), and `report(&self) -> String` (one line per bucket: range, count, percentage).
    - `pub struct Collector { overall: Metrics, per_op: BTreeMap<String, Metrics>, per_phase: BTreeMap<String, Metrics>, hist: LatencyHistogram, raw: Option<BufWriter<File>> }`.
    - `pub fn new(raw_path: Option<&Path>) -> std::io::Result<Collector>` — opens the NDJSON sink when `raw_path` is `Some`.
    - `pub fn add(&mut self, r: &AttackResult, phase: &str, op: Option<&str>)` — if `raw`, write one `serde_json` line (a `RawRecord` view of `r`: seq, code, latency-nanos, bytes, error, method, url); `overall.add(r)`; `hist.add(r.latency)`; `per_op.entry(op).or_default().add(r)` only when `op` is `Some(non-empty)`; `per_phase.entry(phase).or_default().add(r)`. (Go `Add`.)
    - `pub fn close(&mut self)` — `overall.close()` and close every per-op/per-phase `Metrics`; flush `raw`.
    - `pub fn overall(&self) -> &Metrics`, `pub fn per_op(&self) -> &BTreeMap<String, Metrics>` (used by `run`'s summary).
    - `pub fn make_report_dir(base: &str, start: SystemTime) -> std::io::Result<PathBuf>` — create `base/<UTC YYYYMMDDTHHMMSSZ>/`; best-effort refresh a `base/latest` symlink to the new dir's basename (a symlink error is swallowed). (Go `makeReportDir`.)
    - `pub fn write_reports(dir: &Path, c: &Collector, manifest: &RunManifest) -> std::io::Result<()>` — writes `text.txt` (overall summary block), `histogram.txt` (`hist.report()`), `metrics.json` (a `MetricsJson` view of `overall`), `per_operation.json` (always when `per_op` non-empty), `per_phase.json` (only when `per_phase.len() > 1`), and `run_manifest.json`. (Go `writeReports`.)
    - `pub fn write_junit(path: &Path, slos: &[SloResult], elapsed: Duration) -> std::io::Result<()>` — one `<testsuite name="capacity_bench.slos" tests=.. failures=.. time=..>` with a `<testcase name classname="capacity_bench">` per SLO, a `<failure message=..>` on failed ones; XML-escape attribute values. (Go `writeJUnit`.)
    - `#[derive(Serialize)] pub struct OpSummary { name, requests, success, p50, p95, p99, max: String }` and `fn summarize(ms: &BTreeMap<String, Metrics>) -> Vec<OpSummary>` — sorted by name, durations via `crate::slo::fmt_dur`. (Go `opSummary`/`summarize`.)
    - `#[derive(Serialize)] pub struct RunManifest { started_at, finished_at: Option<..>, config_file, config: Config, seed, requests, slo_results: Vec<SloResult>, passed: Option<bool> }` — timestamps as RFC3339-ish strings (`humantime`-free: format epoch seconds, or a small UTC formatter; parity of the exact timestamp string is not required). (Go `runManifest`.)
    - private `MetricsJson` and `RawRecord` serializable views (durations as nanos and/or `fmt_dur` strings; a stable, documented shape — vegeta's JSON schema is not reproduced).
- `src/slo.rs`: add `serde::Serialize` to the existing `SloResult` derive (fields already match Go's json tags: `name`, `target`, `observed`, `passed`).

- [ ] **Step 1: Write the failing tests.** In `report.rs` (a helper `res(code, latency_ms, op_url)` building an `AttackResult`, and a `tmp()` dir):
  - `collector_splits_overall_op_phase`: add results across two phases and two op-URLs (plus one URL matching no op, passed as `op = None`) → `overall().requests` counts all; `per_op()` has exactly the two named ops (no `""` key); a known op's `requests` is right. *(Review Focus: no-op result)*
  - `writes_core_files`: `make_report_dir` under a temp base → dir exists and `base/latest` resolves to it; after `close` + `write_reports`, `text.txt`, `histogram.txt`, `metrics.json`, `per_operation.json`, `run_manifest.json` all exist; `metrics.json` parses and its `requests` matches. *(Review Focus: latest symlink)*
  - `per_phase_only_when_multiphase`: a one-phase collector → no `per_phase.json`; a two-phase collector → `per_phase.json` exists with two entries. *(Review Focus: per_phase gate)*
  - `keep_raw_writes_jsonl`: `Collector::new(Some(path))`, add 3 results, `close` → the file has 3 non-empty JSON lines, each parseable with the expected `code`/`seq`; `Collector::new(None)` writes no file. *(Review Focus: keep_raw)*
  - `junit_marks_failures`: `write_junit` over `[pass, fail]` SLOs → XML contains `tests="2"`, `failures="1"`, exactly one `<failure`, and the failed case's name. *(Review Focus: junit)*
  - `histogram_buckets_monotonic`: feed latencies into known buckets → `report()` lists every bucket and the counts sum to the number of results.
- [ ] **Step 2: Run to verify they fail** — `cargo test -p red-liner report` → FAIL (module missing).
- [ ] **Step 3: Implement** `report.rs` porting `report.go` + `writeJUnit`, and add `Serialize` to `SloResult`. Add `pub mod report;` to `lib.rs`.
- [ ] **Step 4: Run to verify they pass** — `cargo test -p red-liner report` and `cargo test --workspace` → PASS (all prior green).
- [ ] **Step 5: Commit** — `feat(red-liner): per-run report directory (metrics, histogram, per-op/phase, junit, manifest)`.

---

## Task 2: Wire the collector into `run`

Replaces `run`'s single `Metrics` with the `Collector`, attributes each result to its operation, writes the report dir around the attack, and keeps existing runs hermetic.

**Files:**
- Modify: `crates/red-liner/src/encode/mod.rs` (`MixTargeter::operation`), `crates/red-liner/src/run.rs`
- Test: `crates/red-liner/tests/e2e.rs` (existing runs pass `--out`; one new report-dir assertion)
- Reference: Go `main.go` `run()` report scaffolding + measured-phase loop + `printSummary` per-op block.

**Interfaces:**
- Consumes: `Collector`, `make_report_dir`, `write_reports`, `write_junit`, `RunManifest` (Task 1); `MixTargeter` (existing); `Checker` (existing).
- Produces:
  - `src/encode/mod.rs`: `impl MixTargeter { pub fn operation(&self, method: &str, url: &str) -> Option<String> { self.enc.operation(method, url) } }`.
  - `src/run.rs`:
    - After building `targeter`/`phases`, `let start = SystemTime::now();` (keep the `Instant` for elapsed), `let report_dir = report::make_report_dir(&cfg.report.dir, start)?;`, write an initial `RunManifest` (no finished/requests/passed yet) to `report_dir/run_manifest.json`.
    - `let raw = cfg.report.keep_raw.unwrap_or(true).then(|| report_dir.join("results.jsonl"));` `let mut collector = report::Collector::new(raw.as_deref())?;`.
    - Measured-phase loop: `checker.apply(&mut r); let op = targeter.operation(&r.method, &r.url); collector.add(&r, &ph.name, op.as_deref());` (warmup still drains, unchecked, uncollected).
    - After phases: `collector.close();` evaluate SLOs over `collector.overall()`; finalize the `RunManifest` (finished_at, requests, slo_results, passed) and `write_reports(&report_dir, &collector, &manifest)?`; `if cfg.report.junit { write_junit(&report_dir.join("junit.xml"), &results, elapsed)?; }`.
    - `print_summary` gains a per-operation block from `collector.per_op()` (Go `printSummary`), and prints the `report_dir` path at the end.
    - Exit logic unchanged (`enforced() && !passed → SloFailed`).
- [ ] **Step 1: Write the failing tests.**
  - Update existing e2e runs (`e2e_graphql_runs_and_exits_zero`, `e2e_rest_runs_and_exits_zero`, `e2e_slo_violation_exits_one`, `e2e_graphql_errors_fail_slo`, `e2e_harvest_seeds_pool_and_attacks`) to pass `--out <tempdir>` via `args(&cfg, &["--out", dir])` so each writes into its own temp report dir. `validate_flag_exits_zero_without_firing` keeps asserting zero hits (and now also that no report dir was created).
  - Add `e2e_writes_report_dir`: a GraphQL run with `--out <tempdir>` and `report.junit: true` → after `Exit::Ok`, the newest `<tempdir>/<ts>/` holds `text.txt`, `metrics.json`, `per_operation.json`, `run_manifest.json`, `junit.xml`, and (keep_raw default on) `results.jsonl`; `<tempdir>/latest` resolves to it. *(Review Focus: every run writes a dir)*
- [ ] **Step 2: Run to verify they fail** — `cargo test -p red-liner --test e2e` → FAIL (`--out`/`operation`/report wiring missing or dir absent).
- [ ] **Step 3: Implement** `MixTargeter::operation` and the `run.rs` wiring above.
- [ ] **Step 4: Run to verify they pass** — `cargo test --workspace` → PASS (all green; report dirs land in temp paths).
- [ ] **Step 5: Commit** — `feat(red-liner): write the per-run report directory from run`.

---

## Self-Review notes

- **Spec coverage:** `report.go` `collector`/`Add`/`Close`/`newHistogram`/`makeReportDir`/`writeReports`/`summarize`/`runManifest` → Task 1; `slo.go` `writeJUnit`/`junit*` → Task 1; `main.go` report scaffolding + measured-phase `Add` + `printSummary` per-op block → Task 2. The `opOf` resolver (Go passes `enc.operation`) → `MixTargeter::operation` surfaced in Task 2. Config/defaults already shipped — intentionally not re-done.
- **Why no serde in volley:** `metrics.json` is a `red-liner` view over `Metrics`' public fields, so the engine stays serde-free and dependency-stable; the plumbing plan's "volley takes plain Rust types" holds.
- **Divergences (flagged):** raw stream is `results.jsonl` NDJSON (user-confirmed), not vegeta gob; `metrics.json`/`text.txt`/`histogram.txt` are our own stable shapes, not vegeta's reporters (no vegeta in the port); manifest timestamp string format is not byte-for-byte Go.
- **Type consistency:** `Collector`, `Collector::new/add/close`, `make_report_dir`, `write_reports`, `write_junit`, `RunManifest`, `MixTargeter::operation` are named identically across producer (Task 1/2) and consumer (`run`). `SloResult` is `Serialize` once (Task 1) and embedded in the manifest + junit.
- **Review Focus:** six lines — per_phase multi-phase gate, no-op result, keep_raw on/off, junit on/off + counts, latest symlink best-effort, every-run-writes-a-dir (hermetic e2e) — each owned by a named test.
- **Proportion:** signatures + test names/assertions; no Go bodies transcribed — the Go source travels as the spec, cited by function name.
