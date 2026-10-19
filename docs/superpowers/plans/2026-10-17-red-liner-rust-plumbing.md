# Red Liner — Rust Port Plumbing Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. REQUIRED during coding: the `rust-skills` skill (idiomatic ownership, error handling, async).

**Goal:** Stand up the foundational Rust plumbing for Red Liner — a Cargo workspace, a from-scratch load-generation engine that replaces Vegeta (`volley`), config loading, and an end-to-end runnable skeleton that fires a paced REST GET load and gates a CI exit code on a minimal SLO.

**Architecture:** Two crates in a workspace. `volley` is the engine (the Vegeta replacement): protocol-agnostic `Target`/`Result`/`Metrics` types, a `Pacer` trait with constant/poisson/linear strategies, a `Targeter` trait, and an async tokio `Attacker` that fires requests at the paced rate through a reqwest client and streams `Result`s over a channel. `red-liner` is the binary: YAML config model + validation, CLI flags/overrides, and the orchestration that maps config onto the engine and prints a summary with the right exit code. Everything deferred (GraphQL/REST encoders, pools, harvest, checks, disk reports) attaches at named seams — the `Targeter` trait and a `ResultSink` — so later plans add features without reshaping the core.

**Tech Stack:** Rust (edition 2021, MSRV 1.75+), tokio (multi-thread runtime), reqwest (rustls), clap v4 (derive), serde + serde_yaml_ng + serde_json (GraphQL bodies), hdrhistogram, thiserror (libs) + anyhow (binary), rand + rand_chacha (seedable reproducibility). http crate for `HeaderMap`/`Method`.

**Spec:** The existing Go implementation in the repository root (`main.go`, `config.go`, `pacer.go`, `targeter.go`, `protocol.go`, `data.go`, `checks.go`, `slo.go`, `report.go`, `README.md`) is the behavioral spec. This plan ports only the plumbing subset; it cites Go files by name where behavior must match.

## Global Constraints

- Workspace name / tool name: **Red Liner**. Binary name: `red-liner`. Engine crate: `volley`.
- Rust edition 2021; pin an MSRV of 1.75 in each `Cargo.toml` (`rust-version = "1.75"`).
- Config parsing MUST reject unknown keys (`#[serde(deny_unknown_fields)]` on every config struct) — this is the parity requirement for Go's `KnownFields(true)`.
- Durations in config use Go syntax (`"250ms"`, `"2m"`, `"1h30m"`); parse via the `GoDuration` newtype, never a third-party humantime format.
- Exit codes are a hard contract: `0` run passed, `1` SLO violated, `2` config or runtime error. Defined once as an enum in the binary.
- Seeded runs are reproducible: a fixed `load.seed` MUST produce the identical pacing and (later) sampling sequence. Use `rand_chacha::ChaCha8Rng` seeded from the config seed; `seed: 0` means derive from wall-clock once at startup and record the derived value.
- Engine crate `volley` MUST NOT depend on the config crate, clap, or YAML — it takes plain Rust types so it stays reusable and unit-testable.
- Library crates return typed errors (`thiserror`); the binary's `main`/`run` uses `anyhow` and maps to exit codes.

## Review Focus

Inputs the spec implies but no task's happy-path test exercises; each gets a test pinned to the owning task.

- **Unknown/typo'd config key** → must fail config load with a clear error, not be silently ignored. (Task 3)
- **Poisson pacer falling behind schedule** → when observed hits lag `rate × elapsed`, `pace()` must return zero delay so the run catches up rather than drifting below target rate (Go `pacer.go` catch-up branch). (Task 6)
- **Target server times out / connection refused** → the attack must record a `Result` with an error string and keep firing, never panic or abort the run. (Task 8)
- **Attack duration elapses mid-flight** → in-flight requests complete and the result stream closes cleanly; no requests fire after the deadline. (Task 8)
- **Rate of 0 or an empty scenario set reaching the engine** → engine treats non-positive rate as "fire continuously is wrong" → it must stop/err predictably; config validation is the first gate but the engine must not divide-by-zero. (Task 5, Task 6)
- **A required auth env var is unset** → config references `token_env`/`username_env` but the variable is missing → fail before firing with a clear message naming the variable, not a blank header. (Task 8)

---

## File Structure

```
red_liner/                      (workspace root; existing Go files remain until cutover)
  Cargo.toml                    workspace manifest: members = ["crates/volley", "crates/red-liner"]
  rust-toolchain.toml           pin stable channel
  crates/
    volley/
      Cargo.toml
      src/lib.rs                re-exports: Target, Result, Metrics, Pacer, Targeter, Attacker, Error
      src/target.rs             Target, Result
      src/metrics.rs            Metrics (+ hdrhistogram), Latencies
      src/pacer.rs              Pacer trait; ConstantPacer, PoissonPacer, LinearPacer
      src/attacker.rs           Attacker, AttackerConfig, async attack loop
      src/error.rs              volley::Error (thiserror)
    red-liner/
      Cargo.toml
      src/main.rs               entry: exit codes, calls run()
      src/cli.rs                clap Args; flag overrides
      src/config/mod.rs         Config + sub-structs, load/validate/defaults
      src/config/duration.rs    GoDuration newtype + serde + parser
      src/run.rs                orchestration: config -> phases -> engine -> summary -> exit
      src/phases.rs             LoadConfig -> Vec<Phase> (pacer factory)
      src/slo.rs                minimal SLO evaluation + verdict
```

---

## Phase 1 — Foundation (workspace + config)

Standalone deliverable: a building workspace that loads, defaults, validates, and `--validate`s a scenario config. No requests fired yet.

### Task 1: Workspace scaffold

**Files:**
- Create: `Cargo.toml` (workspace), `rust-toolchain.toml`, `crates/volley/Cargo.toml`, `crates/volley/src/lib.rs`, `crates/red-liner/Cargo.toml`, `crates/red-liner/src/main.rs`
- Test: compile + run smoke

**Interfaces:**
- Produces: a workspace that builds; `red-liner` binary that prints its name/version and exits `0`.

- [ ] **Step 1: Write the failing test** — in `crates/red-liner/tests/smoke.rs`, a test that runs the built binary with `--version` and asserts success + that stdout contains `red-liner`.

```rust
#[test]
fn prints_version() {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_red-liner"))
        .arg("--version").output().unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("red-liner"));
}
```

- [ ] **Step 2: Run to verify it fails** — `cargo test -p red-liner` → FAIL (no binary / no clap version).
- [ ] **Step 3: Create the two crate manifests and workspace manifest.** `volley/Cargo.toml` deps: tokio (features `rt-multi-thread`, `macros`, `time`, `sync`), reqwest (`rustls-tls`, no default features), hdrhistogram, rand, rand_chacha, thiserror, futures. `red-liner/Cargo.toml` deps: volley (path), clap (derive), serde (derive), serde_yaml_ng, anyhow, thiserror, tokio. Add `[[bin]] name = "red-liner"`.
- [ ] **Step 4: Minimal `main.rs`** — `#[tokio::main]`, a clap `Parser` with `#[command(name = "red-liner", version)]`, parse and exit `0`. `lib.rs` for volley can be empty re-export stub.
- [ ] **Step 5: Run to verify it passes** — `cargo test -p red-liner` → PASS.
- [ ] **Step 6: Commit** — `chore: scaffold red-liner cargo workspace with volley engine crate`.

---

### Task 2: `GoDuration` newtype and parser

**Files:**
- Create: `crates/red-liner/src/config/duration.rs`
- Test: same file `#[cfg(test)]`

**Interfaces:**
- Produces: `pub struct GoDuration(pub std::time::Duration)`; `impl FromStr`; `Deserialize`/`Serialize` from/to a Go-syntax string; `pub fn as_std(&self) -> std::time::Duration`. Accepts units `ns, us, µs, ms, s, m, h`, signed, compound (`1h30m`), and `"0"`.

- [ ] **Step 1: Write failing tests** — table of cases: `"250ms" -> 250ms`, `"2m" -> 120s`, `"1h30m" -> 5400s`, `"0" -> 0`, `"500µs" -> 500us`, and an error case `"5 minutes" -> Err`.
- [ ] **Step 2: Run to verify it fails** — `cargo test -p red-liner duration` → FAIL.
- [ ] **Step 3: Implement the parser** `impl FromStr for GoDuration`. Approach: scan leading optional sign, then repeating `<number><unit>` groups, summing into a `Duration`; reject trailing/unknown units. Implement `Deserialize` via `deserialize_str` delegating to `FromStr`, `Serialize` via a Go-style formatter (shortest unit is acceptable; parity of output format is not required).
- [ ] **Step 4: Run to verify it passes** → PASS.
- [ ] **Step 5: Commit** — `feat(config): Go-syntax duration newtype`.

---

### Task 3: Config model, defaults, validation, file resolution

**Files:**
- Create: `crates/red-liner/src/config/mod.rs`
- Test: `crates/red-liner/tests/config.rs` + inline unit tests; fixtures under `crates/red-liner/tests/fixtures/`
- Reference: Go `config.go` is the authority for field names, defaults, and validation messages.

**Interfaces:**
- Consumes: `GoDuration` (Task 2).
- Produces:
  - `pub struct Config` with `deny_unknown_fields`, mirroring Go: `version`, `target`, `load`, `data`, `scenarios`, `checks`, `slos`, `report`. Sub-structs `TargetConfig`, `AuthConfig`, `LoadConfig`, `RampConfig`, `Step`, `DataConfig`, `PoolConfig`, `HarvestConfig`, `Scenario`, `RequestConfig`, `ChecksConfig`, `SloConfig`, `ReportConfig` — field-for-field with Go, YAML keys matched via `#[serde(rename)]` where Rust casing differs.
  - `pub enum Protocol { Graphql, Rest }` (serde `rename_all = "lowercase"`).
  - `pub fn load_config_file(path: &Path) -> Result<Config, ConfigError>` — read, decode (deny unknown), `apply_defaults`, `resolve_files(base_dir)`, `validate`. Fail-fast, aggregating all validation errors like Go.
  - `pub fn enforced(&self) -> bool` on `SloConfig`.
  - `pub enum ConfigError` (thiserror): `Io`, `Parse`, `Invalid(Vec<String>)`, `QueryFile`.

- [ ] **Step 1: Write failing tests.**
  - `defaults_applied`: minimal config (url + one scenario) → protocol `graphql`, timeout `10s`, profile `poisson`, rate `200`, duration `30s`, workers `10`, report dir `reports`.
  - `unknown_key_rejected`: a config with `targett:` (typo) → `Err`. *(Review Focus: unknown key)*
  - `missing_url_fails`: no `target.url` → `Err` containing `target.url is required`.
  - `rest_scenario_requires_path` and `graphql_scenario_requires_query`: wrong-protocol fields error with the Go messages.
  - `query_file_resolves_relative_to_config`: a fixture whose scenario uses `query_file: q.graphql` loads the file's contents into `query`.
- [ ] **Step 2: Run to verify they fail** → FAIL.
- [ ] **Step 3: Implement the structs + `apply_defaults` + `validate` + `resolve_files`.** Port defaults and validation branches from `config.go` verbatim (same messages). Use `Option<bool>` for tri-state defaults (`keep_alive`, `graphql_errors`, `keep_raw`, `per_operation`) exactly as Go uses `*bool`. `resolve_files` joins relative `query_file` and file-pool `path` against the config's parent dir; errors if `query` and `query_file` are both set.
- [ ] **Step 4: Run to verify they pass** → PASS.
- [ ] **Step 5: Commit** — `feat(config): scenario config model with defaults and validation`.

---

## Phase 2 — The engine (`volley`, the Vegeta replacement)

Standalone deliverable: a reusable load-generation crate — types, pacers, and an async attacker — unit-tested against a mock server, with no dependency on config or CLI.

### Task 4: `volley` core types — `Target`, `Result`, `Metrics`

**Files:**
- Create: `crates/volley/src/target.rs`, `crates/volley/src/metrics.rs`, `crates/volley/src/error.rs`; wire into `lib.rs`
- Test: inline unit tests in each file
- Reference: Go `vegeta.Target`, `vegeta.Result`, `vegeta.Metrics` (fields listed below).

**Interfaces:**
- Produces:
  - `pub struct Target { pub method: http::Method, pub url: String, pub body: Vec<u8>, pub header: http::HeaderMap }` with `Default`.
  - `pub struct AttackResult { pub attack: String, pub seq: u64, pub code: u16, pub timestamp: SystemTime, pub latency: Duration, pub bytes_in: u64, pub bytes_out: u64, pub error: Option<String>, pub method: String, pub url: String }` — the per-request record (Go `vegeta.Result`). Named `AttackResult` to avoid clashing with `std::result::Result`.
  - `pub struct Metrics` with `add(&mut self, r: &AttackResult)` and `close(&mut self)`, exposing: `requests: u64`, `rate: f64`, `throughput: f64`, `success: f64`, `latencies: Latencies`, `status_codes: BTreeMap<u16, u64>`, `errors: Vec<String>` (deduped), `bytes_in/out`, earliest/latest/end timestamps. `success` = fraction with `error.is_none()` and `code` in 2xx/3xx; `throughput` = successful requests / wall-clock span (Go semantics).
  - `pub struct Latencies { total, mean, p50, p90, p95, p99, max, min: Duration }`, computed in `close()` from an internal `hdrhistogram::Histogram<u64>` of latency-nanos (or microseconds).
  - `pub enum Error` (thiserror) in `error.rs`: `Http`, `Timeout`, `Build`.

- [ ] **Step 1: Write failing tests** — `metrics_success_and_percentiles`: add 100 synthetic `AttackResult`s (90 code 200, 10 with an error), `close()`, assert `requests == 100`, `success == 0.9`, `latencies.p50 > 0`, `status_codes[&200] == 90`, `errors.len() == 1` after dedup. `throughput_counts_only_success`: assert throughput reflects 90 over the span.
- [ ] **Step 2: Run to verify it fails** → FAIL.
- [ ] **Step 3: Implement `Target`, `AttackResult`, `Metrics`/`Latencies`.** `Metrics::add` records latency into the histogram, bumps counters, tracks min/max timestamps, dedups error strings. `close` finalizes percentiles and computes rate/throughput from the timestamp span.
- [ ] **Step 4: Run to verify it passes** → PASS.
- [ ] **Step 5: Commit** — `feat(volley): core Target/Result/Metrics types`.

---

### Task 5: `volley` `Pacer` and `Targeter` traits + constant/linear pacers

**Files:**
- Create: `crates/volley/src/pacer.rs`; extend `lib.rs`
- Test: inline unit tests
- Reference: Go `vegeta.Pacer` interface; `ConstantPacer` (`vegeta.Rate`) and `LinearPacer`.

**Interfaces:**
- Produces:
  - `pub trait Pacer: Send + Sync { fn pace(&self, elapsed: Duration, hits: u64) -> (Duration, bool); fn rate(&self, elapsed: Duration) -> f64; }` — return `(wait, stop)`; `stop == true` ends the attack (Go's bool is "stop").
  - `pub trait Targeter: Send + Sync { fn target(&self, t: &mut Target) -> Result<(), Error>; }` — fills one request per hit.
  - `pub struct ConstantPacer { pub freq: u64, pub per: Duration }` and `pub struct LinearPacer { pub start_freq: f64, pub slope_per_sec: f64 }`, both `impl Pacer`.
- Consumes: `Target`, `Error` (Task 4).

- [ ] **Step 1: Write failing tests** — `constant_pacer_targets_rate`: a 100 rps constant pacer, simulate the Go loop (accumulate waits until `elapsed >= 1s`) and assert ~100 hits. `linear_pacer_increases`: assert `rate(10s) > rate(0s)`.
- [ ] **Step 2: Run to verify it fails** → FAIL.
- [ ] **Step 3: Implement the traits and the two deterministic pacers.** Port the constant-rate "expected hits vs elapsed" math and the linear ramp slope from Go (`pacer.go`, `vegeta.Rate`/`LinearPacer`). Non-positive freq → `pace` returns `(0, true)` (stop) to satisfy the Review-Focus zero-rate case.
- [ ] **Step 4: Run to verify it passes** → PASS.
- [ ] **Step 5: Commit** — `feat(volley): Pacer and Targeter traits with constant/linear pacers`.

---

### Task 6: `volley` `PoissonPacer`

**Files:**
- Modify: `crates/volley/src/pacer.rs`
- Test: inline
- Reference: Go `PoissonPacer` in `pacer.go` (exponential inter-arrival + catch-up).

**Interfaces:**
- Produces: `pub struct PoissonPacer { mean_rate: f64, rng: Mutex<ChaCha8Rng> }`; `pub fn new(rate: f64, seed: u64) -> Self`; `impl Pacer`.
- Consumes: `Pacer` trait (Task 5).

- [ ] **Step 1: Write failing tests** — `poisson_mean_converges`: drive the pacer through a simulated 10s at mean 200 rps using a seeded rng, assert total hits within ±5% of 2000. `poisson_catches_up`: with `hits` far below `mean_rate * elapsed`, `pace` returns zero wait. *(Review Focus: catch-up)*
- [ ] **Step 2: Run to verify it fails** → FAIL.
- [ ] **Step 3: Implement.** `pace`: if `mean_rate <= 0` → `(0, true)`; if `hits < mean_rate * elapsed_secs` → `(ZERO, false)` (catch-up); else draw `u` from the rng and return `(-ln(u)/mean_rate seconds, false)`. Guard `u` away from zero. `rate` returns `mean_rate`.
- [ ] **Step 4: Run to verify it passes** → PASS.
- [ ] **Step 5: Commit** — `feat(volley): seeded Poisson pacer with catch-up`.

---

### Task 7: `volley` `Attacker` — async paced attack loop

**Files:**
- Create: `crates/volley/src/attacker.rs`; extend `lib.rs`
- Test: inline test against a local `tokio`/`hyper` mock server (dev-dependency `wiremock` or a hand-rolled `tokio::net::TcpListener` echo)
- Reference: Go `vegeta.Attacker.Attack` and `main.go`'s per-phase attacker usage.

**Interfaces:**
- Produces:
  - `pub struct AttackerConfig { pub timeout: Duration, pub workers: usize, pub max_workers: usize, pub max_idle_conns: usize, pub keep_alive: bool, pub http2: bool }`.
  - `pub struct Attacker { client: reqwest::Client, cfg: AttackerConfig }`; `pub fn new(cfg: AttackerConfig) -> Result<Self, Error>` (builds the reqwest client with pool/timeout/keepalive settings).
  - `pub fn attack<P, T>(&self, targeter: Arc<T>, pacer: Arc<P>, duration: Duration, name: String) -> tokio::sync::mpsc::Receiver<AttackResult>` where `P: Pacer + 'static`, `T: Targeter + 'static`. Spawns a scheduler task that consults the pacer, dispatches each hit to a worker task (bounded by `max_workers` via a `Semaphore`), times the request, and sends an `AttackResult` per hit; closes the channel when `duration` elapses or the pacer returns stop.
- Consumes: `Target`, `AttackResult`, `Pacer`, `Targeter`, `Error`.

- [ ] **Step 1: Write failing tests.**
  - `attack_fires_at_rate`: against a mock 200-OK server, a 2s constant 50 rps attack yields ~100 results, all `code == 200`.
  - `attack_records_transport_error`: point at a closed port → results carry `error.is_some()`, the loop does not panic and still returns. *(Review Focus: connection refused)*
  - `attack_stops_at_deadline`: no results arrive after `duration`; channel closes. *(Review Focus: duration elapses)*
- [ ] **Step 2: Run to verify it fails** → FAIL.
- [ ] **Step 3: Implement the attack loop.** Scheduler loop: track `elapsed`/`hits`; call `pacer.pace`; if stop or `elapsed >= duration`, break; if wait > 0 `tokio::time::sleep(wait)`; else acquire a semaphore permit and `tokio::spawn` a worker that calls `targeter.target`, builds the reqwest request, awaits it under `timeout`, measures latency, maps success/error/body-size into `AttackResult`, and sends it. Record `seq` monotonically. Drain outstanding permits before closing the channel.
- [ ] **Step 4: Run to verify it passes** → PASS.
- [ ] **Step 5: Commit** — `feat(volley): async paced attacker over reqwest`.

---

## Phase 3 — End-to-end wiring, GraphQL-first (config → engine → verdict)

Standalone deliverable: `red-liner -config graphql.yaml` fires a paced **GraphQL** load — a weighted mix of operations with auth headers — prints a summary, and exits with the SLO-gated code. The full plumbing path proven end to end against a mock GraphQL server. REST is the next plan.

### Task 8: GraphQL encoder, auth headers, weighted mix, GraphQL targeter

**Files:**
- Create: `crates/red-liner/src/encode/mod.rs`, `crates/red-liner/src/encode/graphql.rs`, `crates/red-liner/src/encode/headers.rs`, `crates/red-liner/src/mix.rs`
- Test: inline unit tests in each file
- Reference: Go `protocol.go` (`baseHeaders`, `graphqlEncoder`, `newGraphQLEncoder`), `targeter.go` (`weightedMix`, `NewTargeter`).

**Interfaces:**
- Consumes: `Config`/`Scenario`/`TargetConfig`/`AuthConfig` (Task 3); `Target`, `Targeter` trait, `AttackResult`, `Error` (Tasks 4–5).
- Produces:
  - `src/encode/headers.rs`: `pub fn base_headers(cfg: &TargetConfig) -> Result<http::HeaderMap, EncodeError>` — sets `Accept: application/json`, user headers, and resolves auth from env (`none|bearer|basic|header`) exactly as Go `baseHeaders` (basic → base64 `user:pass`; bearer → `Authorization: Bearer <tok>`; header → `<name>: <tok>`). `EncodeError::MissingEnv(String)` for an unset required env var.
  - `src/mix.rs`: `pub struct WeightedMix { scenarios: Vec<Scenario>, cum: Vec<f64> }`; `pub fn new(scenarios: Vec<Scenario>) -> Self`; `pub fn pick(&self, rng: &mut impl Rng) -> &Scenario` — cumulative-weight table + one draw, binary search (Go `weightedMix`).
  - `src/encode/graphql.rs`: `pub struct GraphqlEncoder { endpoint: String, headers: http::HeaderMap }`; `pub fn new(cfg: &TargetConfig) -> Result<Self, EncodeError>` (adds `Content-Type: application/json`); `pub fn encode(&self, t: &mut Target, sc: &Scenario)` — POST, body = `serde_json` of `{operationName, query, variables}`, URL = `endpoint?op=<url-escaped name>`; `pub fn operation(&self, url: &str) -> Option<String>` parses the `?op=` tag back (Go `graphqlEncoder`). Variables are serialized from the config as-is (static). `// SEAM: {{placeholder}} pool templating replaces the static variables pass in the data-pools plan.`
  - `src/encode/mod.rs`: `pub struct GraphqlTargeter { enc: GraphqlEncoder, mix: WeightedMix, rng: Mutex<ChaCha8Rng> }` implementing `volley::Targeter`; `pub fn new(enc, mix, seed) -> Self`. `target()` locks the rng, picks a scenario, encodes it. Reproducible from one seed (Go `NewTargeter`).

- [ ] **Step 1: Write failing tests.**
  - `base_headers_bearer_from_env`: with `BENCH_TOKEN=abc` in env and `auth.type = bearer`, header `Authorization == "Bearer abc"`; missing env → `Err(MissingEnv)`. *(Review Focus: missing auth env)*
  - `base_headers_basic_base64`: basic auth with username/password envs → correct base64 credential.
  - `weighted_mix_respects_weights`: a seeded rng over a 90/10 two-scenario mix across 10k picks lands ~90/10 (±5%); a single-scenario mix always returns it.
  - `graphql_encode_builds_post`: `encode` sets POST, `?op=GetProduct` in the URL, and a body whose parsed JSON has `operationName == "GetProduct"` and the scenario's `query`. `operation()` round-trips the op name.
- [ ] **Step 2: Run to verify they fail** → FAIL.
- [ ] **Step 3: Implement headers, mix, encoder, targeter.** Port `baseHeaders`, `weightedMix`/`pick`, `graphqlEncoder.encode`/`operation`, and `NewTargeter` from Go. `EncodeError` is `thiserror`; map it into `volley::Error` at the `Targeter` boundary.
- [ ] **Step 4: Run to verify they pass** → PASS.
- [ ] **Step 5: Commit** — `feat(red-liner): GraphQL encoder, auth, weighted mix, targeter`.

### Task 9: Binary orchestration — phase factory, minimal SLO, run loop, exit codes

**Files:**
- Create: `crates/red-liner/src/cli.rs`, `crates/red-liner/src/phases.rs`, `crates/red-liner/src/slo.rs`, `crates/red-liner/src/run.rs`; rewrite `crates/red-liner/src/main.rs`
- Test: `crates/red-liner/tests/e2e.rs` (against a mock GraphQL server), plus inline unit tests for `phases` and `slo`
- Reference: Go `main.go` (`run`, flags, warmup+phases loop, exit codes), `pacer.go` `buildPhases`, `slo.go` `evaluateSLOs`.

**Interfaces:**
- Consumes: `Config` + `load_config_file` (Task 3); all of `volley`; `GraphqlEncoder`/`WeightedMix`/`GraphqlTargeter` (Task 8).
- Produces:
  - `src/cli.rs`: `pub struct Args` (clap) with `config`, `url`, `rate`, `duration`, `warmup`, `seed`, `out`, `validate`; `pub fn apply_overrides(&self, cfg: &mut Config)` that overrides only flags the user set (clap `Option` fields; `Some` means set — the parity rule for Go's `fs.Visit`).
  - `src/phases.rs`: `pub struct Phase { pub name: String, pub pacer: Arc<dyn Pacer>, pub duration: Duration, pub rate: f64 }`; `pub fn build_phases(load: &LoadConfig) -> Result<Vec<Phase>, anyhow::Error>` mapping `constant|poisson|ramp|steps` onto pacers (Go `buildPhases`).
  - `src/slo.rs`: `pub struct SloResult { name, target, observed, passed }`; `pub fn evaluate(slos: &SloConfig, m: &Metrics) -> Vec<SloResult>`; `pub fn passed(&[SloResult]) -> bool` (Go `slo.go`, minus JUnit).
  - `src/run.rs`: `pub async fn run(args: Args) -> Exit`. Loads config; on `--validate` prints the Go-style line and returns `Ok`; builds the `GraphqlTargeter` (Task 8) and phases; runs an unmeasured warmup if `load.warmup > 0`; runs each phase with a fresh `Attacker`, draining its receiver into one `Metrics`; `close()`; evaluates SLOs; prints the summary (elapsed/requests/rate/throughput/success/p50/p95/p99 + SLO table, matching `printSummary`); returns the exit code. The targeter is an `Arc<dyn Targeter>` so the deferred REST targeter drops in without touching `run`.
  - `src/main.rs`: `pub enum Exit { Ok = 0, SloFailed = 1, Error = 2 }`; `#[tokio::main]` maps `run`'s outcome to `std::process::exit`.

- [ ] **Step 1: Write failing tests.**
  - `build_phases_steps_expands`: a steps profile with 3 steps → 3 phases with matching rates/durations; `poisson` and `ramp` → 1 phase each.
  - `slo_fails_on_latency`: metrics with p95 above the configured p95 → `passed() == false` and that result's `passed == false`.
  - `e2e_graphql_runs_and_exits_zero`: temp GraphQL config pointing at a mock server returning `200 {"data":{}}`, `constant` 50 rps for 1s, no SLOs → `run` returns `Ok`, non-zero request count, mock saw POSTs with a GraphQL body.
  - `e2e_slo_violation_exits_one`: same run with an impossible SLO (`throughput_min: 1_000_000`) → returns `SloFailed`.
  - `validate_flag_exits_zero_without_firing`: `--validate` on a good config returns `Ok` and fires nothing (mock server receives zero requests).
- [ ] **Step 2: Run to verify they fail** → FAIL.
- [ ] **Step 3: Implement `cli`, `phases`, `slo`, and `run`/`main`.** Port `buildPhases`, `evaluateSLOs`/`slosPassed`, the warmup (`max(1, rate/10)` rps uniform, unmeasured), the per-phase fresh-attacker loop, and `printSummary` formatting from Go. Wire exit codes; `enforced() && !passed` → `SloFailed`.
- [ ] **Step 4: Run to verify they pass** → `cargo test --workspace` → PASS.
- [ ] **Step 5: Commit** — `feat(red-liner): end-to-end GraphQL run with SLO gate and exit codes`.

---

## Deferred to later plans (seams left in place)

- **REST encoder** (`protocol.go` REST half + path-pattern `operation` matching) behind the same `Targeter` seam — the immediate next plan.
- Data pools (list/file/range), `{{placeholder}}` templating, and native-type preservation — replaces Task 8's static-variables pass.
- Harvest / bootstrap probe and JSON-path extraction.
- App-layer checks (status set + GraphQL `errors`) — a result-reclassification `ResultSink` seam between attacker and collector.
- Disk reports: per-operation, per-phase, histogram, `metrics.json`, `results.bin` raw stream, `run_manifest.json`, JUnit XML, `reports/<timestamp>/` + `latest` symlink.
- HTTP/2, read/write buffer tuning, and any hyper-direct path if reqwest throughput proves insufficient.

## Self-Review notes

- **Spec coverage (plumbing subset):** config model+validation (Task 3) ← `config.go`; durations (Task 2) ← `config.go`; Target/Result/Metrics (Task 4) ← vegeta types used across `report.go`/`main.go`; pacers (Tasks 5–6) ← `pacer.go`; attacker (Task 7) ← vegeta `Attack`; GraphQL encoder + auth + mix + targeter (Task 8) ← `protocol.go`/`targeter.go`; phases+SLO+exit+orchestration (Task 9) ← `main.go`/`slo.go`. Deferred items (REST next) are enumerated above, not silently dropped.
- **Type consistency:** `AttackResult`, `Metrics`, `Pacer`, `Targeter`, `Target`, `Phase`, `GraphqlTargeter` names are used identically in producing and consuming tasks. `success`/`throughput` semantics defined once in Task 4 and consumed by Task 9's SLO. `run` depends on `Arc<dyn Targeter>`, not on `GraphqlTargeter` concretely.
- **Review Focus:** each line has an owning task + test — unknown key (Task 3), Poisson catch-up (Task 6), transport error (Task 7), deadline (Task 7), zero rate (Tasks 5/6), missing auth env (Task 8).
- **Proportion:** signatures and test assertions only; no function bodies transcribed.
