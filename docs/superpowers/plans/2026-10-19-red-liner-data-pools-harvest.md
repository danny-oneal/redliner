# Red Liner — Data Pools, Templating & Harvest Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. REQUIRED during coding: the `rust-skills` skill (idiomatic ownership, error handling, enums over downcast).

**Goal:** Fill the `{{placeholder}}` SEAMs left in both encoders so scenarios draw request values from named pools, and add a harvest pass that probes the live GraphQL/REST server before the attack and seeds those pools with real identifiers via JSON-path extraction — so every fired request resolves to a record that actually exists.

**Architecture:** Port Go's `data.go` (pools, templating, JSON-path extract) and `bootstrap.go` (harvest) behind the existing encoder seam. A new `data` module owns a `Pool` enum (`List`/`Range`), a `Pools` registry, the `{{pool}}` templating functions, and `extract_path`. `Encoder::encode` grows `&Pools` + `&mut ChaCha8Rng` so the encoders render at attack time; `MixTargeter` owns the `Pools` and passes its already-locked rng through. Harvest runs once in `run` before the attack: an async reqwest one-shot client (the project is already async; Go's blocking client is adapted) reusing `base_headers`, fanning each response through `extract_path`, appending into the named pool. Config (`DataConfig`/`PoolConfig`/`HarvestConfig`, validation, file-path resolution) already shipped and is **not** re-implemented.

**Tech Stack:** Rust (edition 2021, MSRV 1.88). New dependency in `red-liner`: `reqwest` for the harvest client (matching `volley`'s pin). Existing: `serde_json` (pool values / JSON walking), `rand_chacha` (seeded rng), `http`, `thiserror`.

**Spec:** The Go implementation is the behavioral authority — `data.go` (`Pool`, `listPool`, `rangePool`, `buildPools`, `readPoolFile`, `normalizeScalar`, `extractPath`, `renderString`, `renderValue`, `stringify`) and `bootstrap.go` (`bootstrapClient`, `newBootstrapClient`, `runHarvest`, `fetch`). This plan is the "data pools … `{{placeholder}}` templating … harvest / bootstrap probe and JSON-path extraction" item deferred in `docs/superpowers/plans/2026-10-17-red-liner-rust-plumbing.md` and the SEAMs marked in `encode/graphql.rs:32` and `encode/rest.rs:57`.

## Global Constraints

- Config parsing/defaults/validation for `data.pools` and `data.harvest` already exist in `config/mod.rs` and are **not** re-done: `DataConfig { pools: BTreeMap<String, PoolConfig>, harvest: Vec<HarvestConfig> }`, `PoolConfig { pool_type, values: Vec<Value>, path, min, max, as_type }`, `HarvestConfig { request: RequestConfig, extract: BTreeMap<String, String> }`. File-pool paths are already resolved to absolute against the config dir. Treat these as given.
- `volley` MUST NOT gain any dependency from this work — all code lives in the `red-liner` binary.
- Exit-code contract unchanged: `0` pass, `1` SLO violated, `2` config/runtime error. A harvest failure is a runtime error (exit `2`), surfaced before any attack traffic.
- Seeded runs stay reproducible: the single `Mutex<ChaCha8Rng>` in `MixTargeter` is the only sampling rng; pool sampling draws from it in encode order.
- Native-type preservation: a JSON/YAML number that is integral stays an integer end-to-end (GraphQL `Int!` variables must not be sent as floats). This is Go `normalizeScalar`, applied at pool-build, at extract, and at sample.
- New `reqwest` dep: match `volley`'s declaration exactly — `reqwest = { version = "0.12", default-features = false, features = ["rustls-tls"] }` — so the workspace resolves one version.
- `Pool` is modeled as an `enum { List(Vec<Value>), Range { min, max } }`, not a `dyn Pool` trait object: harvest must append into the `List` variant, which a Rust enum match does cleanly and a trait object would need downcasting for. (Plan author's call; diverges from Go's interface, same behavior.)

## Review Focus

Inputs the Go spec implies that the happy-path tests never exercise; each is pinned to a task.

- **A variable that is exactly one placeholder keeps its native type** → `id: "{{ids}}"` where `ids` samples an int must encode as JSON `5`, not `"5"`; a placeholder embedded mid-string (`"/u/{{ids}}/x"`) stringifies. (Task 1)
- **Unknown pool name in a template** → encode fails with the pool name, rather than emitting a literal `{{typo}}` or panicking. (Task 1)
- **A harvested JSON number that is integral** → `extract_path` on a value decoded as `5.0` yields int `5`, so templated IDs are `5` not `5.0`. (Task 1)
- **Harvest path matches nothing** → error naming the pool and path ("schema mismatch?"), not a silent empty pool that makes every later `sample` panic on an empty list. (Task 2)
- **Harvest into a pool name that a static pool already defined** → values append to the existing list, they do not replace it. (Task 2)

---

## File Structure

```
crates/red-liner/
  Cargo.toml                 + reqwest dependency (Task 2)
  src/data/mod.rs            NEW: Pool, Pools, build_pools, read_pool_file, normalize_scalar,
                                  render_string, render_value, stringify, extract_path (Task 1)
  src/data/harvest.rs        NEW: BootstrapClient, run_harvest (Task 2)
  src/lib.rs                 + pub mod data; (Task 1)
  src/encode/mod.rs          Encoder::encode gains (&Pools, &mut ChaCha8Rng); MixTargeter owns Pools (Task 1)
  src/encode/graphql.rs      encode renders variables via render_value (Task 1)
  src/encode/rest.rs         encode renders path/body via render_string (Task 1)
  src/run.rs                 build_pools at startup (Task 1), then run_harvest before the attack (Task 2)
  tests/e2e.rs               + harvest e2e + a mock returning a seedable JSON payload (Task 2)
```

---

## Task 1: `data` module — pools, templating, extract; thread pools+rng through the encoder seam

Ports `data.go`. Deliverable: static pools (`list`/`range`/`file`) render into GraphQL variables and REST path/body end-to-end, reproducibly from a seed; `extract_path` is implemented and unit-tested (consumed by Task 2). No harvest yet.

**Files:**
- Create: `crates/red-liner/src/data/mod.rs`
- Modify: `crates/red-liner/src/lib.rs` (`pub mod data;`), `crates/red-liner/src/encode/mod.rs`, `crates/red-liner/src/encode/graphql.rs`, `crates/red-liner/src/encode/rest.rs`, `crates/red-liner/src/run.rs`
- Test: inline tests in `data/mod.rs`; updated inline tests in `encode/mod.rs`, `encode/graphql.rs`, `encode/rest.rs`
- Reference: Go `data.go`.

**Interfaces:**
- Consumes: `DataConfig`, `PoolConfig` (config, existing); `serde_json::Value`; `rand_chacha::ChaCha8Rng`, `rand::Rng`; `EncodeError` (existing).
- Produces:
  - `src/data/mod.rs`:
    - `pub enum Pool { List(Vec<serde_json::Value>), Range { min: i64, max: i64 } }` with `pub fn sample(&self, rng: &mut ChaCha8Rng) -> serde_json::Value` (list: `values[rng.random_range(0..len)]`; range: `Value` int in `min..=max`).
    - `pub type Pools = std::collections::HashMap<String, Pool>;`
    - `pub fn build_pools(cfg: &DataConfig) -> Result<Pools, DataError>` — list → `Pool::List(normalize)`, range → `Pool::Range`, file → `read_pool_file`. Pools are keyed by name.
    - `fn read_pool_file(path: &str, as_type: &str) -> Result<Vec<Value>, DataError>` — port `readPoolFile`: trim lines, skip blank and `#` comments, `as == "int"` parses to int Value else string Value, error on empty file.
    - `pub fn normalize_scalar(v: &Value) -> Value` — integral float Number → integer Number; else clone. (Go `normalizeScalar`.)
    - `pub fn render_string(s: &str, pools: &Pools, rng: &mut ChaCha8Rng) -> Result<String, EncodeError>` — replace each `{{name}}` with `stringify(sample)`; `EncodeError::UnknownPool(name)` if absent, `EncodeError::BadTemplate(s)` on unterminated `{{`.
    - `pub fn render_value(v: &Value, pools: &Pools, rng: &mut ChaCha8Rng) -> Result<Value, EncodeError>` — recurse objects/arrays; a string that trims to exactly one `{{pool}}` returns `pool.sample(..)` with native type, otherwise `render_string`; other scalars → `normalize_scalar`. (Go `renderValue`.)
    - `fn stringify(v: &Value) -> String` — string→raw, int/float→plain number text (no quotes), else `to_string`.
    - `pub fn extract_path(doc: &Value, path: &str) -> Result<Vec<Value>, DataError>` — port `extractPath`: split on `.`; a segment ending `[]` fans out over an array, else indexes the object; tolerate missing/null (skip), error on type mismatch; `normalize_scalar` every leaf.
    - `pub enum DataError` (thiserror): `Io`, `NotInt(String)`, `EmptyFile(String)`, `Path { path: String, msg: String }` — and (Task 2 adds harvest variants). Maps to `anyhow` in `run`.
  - `src/encode/mod.rs`:
    - `Encoder::encode` becomes `fn encode(&self, t: &mut Target, sc: &Scenario, pools: &Pools, rng: &mut ChaCha8Rng) -> Result<(), EncodeError>;` (`operation` unchanged).
    - `MixTargeter` gains `pools: Pools`; `MixTargeter::new(enc, mix, pools: Pools, seed: u64)`; `target()` passes `&self.pools` and `&mut *rng` into `encode`.
    - `EncodeError` gains `#[error("unknown pool {0}")] UnknownPool(String)` and `#[error("bad template: {0}")] BadTemplate(String)`.
  - `src/encode/graphql.rs`: `encode` builds `variables` via `render_value(&Value::Object(sc.request.variables.clone().into_iter().collect()), pools, rng)?` before marshaling.
  - `src/encode/rest.rs`: `encode` sets `t.url = format!("{}{}", self.base_url, render_string(&sc.request.path, pools, rng)?)` and, when body non-empty, `render_string(&sc.request.body, pools, rng)?`.
  - `src/run.rs`: `let pools = data::build_pools(&cfg.data)?;` then `MixTargeter::new(enc, mix, pools, cfg.load.seed as u64)`.

- [ ] **Step 1: Write the failing tests.**
  - In `data/mod.rs`:
    - `render_value_keeps_native_type`: pool `ids = Pool::List(vec![json!(5)])`; `render_value(&json!({"id":"{{ids}}"}), ..)` → `json!({"id":5})` (number, not string); `render_value(&json!({"p":"/u/{{ids}}/x"}), ..)` → `json!({"p":"/u/5/x"})`. *(Review Focus: native type)*
    - `render_string_unknown_pool`: `render_string("{{nope}}", &empty, ..)` → `Err(EncodeError::UnknownPool("nope"))`. *(Review Focus: unknown pool)*
    - `render_string_unterminated`: `render_string("a {{ids", ..)` → `Err(EncodeError::BadTemplate(_))`.
    - `range_pool_bounds`: `Pool::Range{min:1,max:3}.sample` over many draws stays in `1..=3` and yields integer Values.
    - `extract_path_fans_out`: `extract_path(&json!({"data":{"items":[{"id":1},{"id":2}]}}), "data.items[].id")` → `vec![json!(1), json!(2)]`.
    - `extract_path_normalizes_integral_float`: `extract_path(&json!({"id":5.0}), "id")` → `vec![json!(5)]` (int, not `5.0`). *(Review Focus: integral float)*
    - `build_pools_file_as_int`: a temp file with `1`, `# c`, blank, `2` and `as:"int"` → `Pool::List(vec![json!(1), json!(2)])`.
  - In `encode/rest.rs`: `rest_encode_renders_path` — scenario `path: "/products/{{id}}"`, pool `id=Pool::List(vec![json!(5)])` → `t.url == "http://h/products/5"`.
  - In `encode/graphql.rs`: `graphql_encode_renders_variables` — scenario var `{"id":"{{id}}"}`, pool `id=Pool::List(vec![json!(5)])` → decoded body `variables.id == 5` (number).
  - In `encode/mod.rs`: update `targeter_is_reproducible_from_seed` to pass a `Pools` (empty or a 1-value list) to `MixTargeter::new`, and update the two `operation` call sites only if the signature moved (it did not).
- [ ] **Step 2: Run to verify they fail** — `cargo test -p red-liner` → FAIL to compile (`data` module / `render_*` / new `encode` arity missing).
- [ ] **Step 3: Implement.** Add `pub mod data;` to `lib.rs`. Write `data/mod.rs` porting `data.go` (functions above; `render_string`/`render_value`/`extract_path`/`normalize_scalar` follow the Go bodies). Change the `Encoder` trait arity, `MixTargeter` (add `pools` field + ctor param), both encoders' `encode`, and `run.rs` (`build_pools` + pass pools). Add the two `EncodeError` variants.
- [ ] **Step 4: Run to verify they pass** — `cargo test --workspace` → PASS (all prior tests green through the new seam; GraphQL/REST e2e unchanged because empty `data.pools` renders static strings unchanged).
- [ ] **Step 5: Commit** — `feat(red-liner): data pools, {{placeholder}} templating, and JSON-path extract`.

---

## Task 2: Harvest — bootstrap probe + append into pools, wired into `run`

Ports `bootstrap.go`. Deliverable: a config with `data.harvest` probes the live server before the attack, extracts IDs by JSON-path, appends them into the named pool, and the attack fires against those real values.

**Files:**
- Create: `crates/red-liner/src/data/harvest.rs`
- Modify: `crates/red-liner/src/data/mod.rs` (`pub mod harvest;`, add harvest `DataError` variants), `crates/red-liner/src/run.rs` (call `run_harvest` after `build_pools`, before the attack), `crates/red-liner/Cargo.toml` (+`reqwest`)
- Test: inline tests in `harvest.rs` (append semantics); one harvest e2e in `tests/e2e.rs` + a mock returning a seedable JSON payload
- Reference: Go `bootstrap.go`.

**Interfaces:**
- Consumes: `Pools`, `Pool`, `extract_path`, `DataError` (Task 1); `Config`, `TargetConfig`, `RequestConfig`, `Protocol` (config); `base_headers` (`encode::headers`); `reqwest::Client`.
- Produces:
  - `src/data/harvest.rs`:
    - `struct BootstrapClient { target: TargetConfig, client: reqwest::Client, headers: http::HeaderMap }`.
    - `fn new(cfg: &TargetConfig) -> Result<BootstrapClient, DataError>` — `reqwest::Client::builder().timeout(cfg.timeout)...`, headers = `base_headers(cfg)?`.
    - `async fn fetch(&self, proto: Protocol, rc: &RequestConfig) -> Result<serde_json::Value, DataError>` — GraphQL: POST `target.url` with `{"query","variables"}`, `Content-Type: application/json`; REST: `method` (default GET, upper-cased) to `trim_right('/')+path`, body + `Content-Type` only if body non-empty. Non-200 → error; decode JSON; GraphQL `errors` present → error (reuse existing graphql-errors check). (Go `fetch`.)
    - `pub async fn run_harvest(cfg: &Config, pools: &mut Pools) -> Result<(), DataError>` — no-op if `cfg.data.harvest` empty; for each harvest, `fetch` then for each `(pool_name, path)` `extract_path`, error if the match is empty ("schema mismatch?"), then append into an existing `Pool::List` or insert a new one. (Go `runHarvest`.)
    - `DataError` gains `Http(String)`, `Status { code: u16, url: String }`, `Decode(String)`, `GraphqlErrors(String)`, `EmptyMatch { pool: String, path: String }`.
  - `src/run.rs`: after `build_pools`, `data::harvest::run_harvest(&cfg, &mut pools).await?;` (map `DataError` → `anyhow`, exit `2`) before constructing the targeter.
- [ ] **Step 1: Write the failing tests.**
  - In `harvest.rs`:
    - `run_harvest_appends_to_existing_pool`: seed `pools` with `ids = Pool::List(vec![json!(1)])`; a harvest whose `extract` maps `ids` and whose fetched doc yields `[2,3]` (drive `extract_path` directly, or a tiny injectable fetch) → `ids` becomes `[1,2,3]`. *(Review Focus: append not replace)*
    - `run_harvest_empty_match_errors`: a path that matches nothing → `Err(DataError::EmptyMatch{..})`. *(Review Focus: no-match error)*
  - In `tests/e2e.rs`: `e2e_harvest_seeds_pool_and_attacks` — a mock that returns `{"data":{"products":[{"id":7}]}}` to the harvest request and records attack request lines; config with `data.harvest: [{ request:{...}, extract:{ ids: "data.products[].id" } }]` and a scenario `path: "/products/{{ids}}"`, `constant 50 rps` for `1s`, no SLOs → `run` returns `Ok`, and a recorded attack line is `GET /products/7 HTTP/1.1`.
- [ ] **Step 2: Run to verify they fail** — `cargo test -p red-liner harvest` and `cargo test -p red-liner --test e2e e2e_harvest` → FAIL (module / `reqwest` missing).
- [ ] **Step 3: Implement.** Add `reqwest` to `Cargo.toml` (matching `volley`). Write `harvest.rs` porting `bootstrap.go` (async reqwest adapting Go's blocking client). Add the harvest `DataError` variants. Wire `run_harvest` into `run.rs` after `build_pools`. For the e2e mock, extend the existing listener so the first request (harvest) answers with the seed payload and later requests answer `200 {"data":{}}` while recording their request lines.
- [ ] **Step 4: Run to verify they pass** — `cargo test --workspace` → PASS (GraphQL + REST + harvest e2e green).
- [ ] **Step 5: Commit** — `feat(red-liner): harvest bootstrap probe seeds pools from live server`.

---

## Self-Review notes

- **Spec coverage:** `data.go` → Task 1 (`Pool`/`build_pools`/`read_pool_file`/`normalize_scalar`/`render_string`/`render_value`/`stringify`/`extract_path`); `bootstrap.go` → Task 2 (`BootstrapClient`/`fetch`/`run_harvest`). Encoder seam change (Go threads `pools, rng` into `encode`) → Task 1. Config/validation already shipped — intentionally not re-done.
- **Step scan:** every code step gives a signature + file; the only non-obvious algorithms (`render_value` native-type rule, `extract_path` fan-out, `normalize_scalar`) are named ports of specific Go functions with their behavior pinned by test assertions carrying exact values, not transcribed bodies.
- **Type consistency:** `Pool`, `Pools`, `build_pools`, `render_string`, `render_value`, `extract_path`, `run_harvest`, `DataError`, and the new `encode(&self, t, sc, pools, rng)` arity are named identically across producer (Task 1/2) and consumer (encoders, `run`). `MixTargeter::new` takes `Pools` in both the ctor definition (Task 1) and its `run.rs` call site.
- **Review Focus:** five lines — native-type preservation (Task 1), unknown pool (Task 1), integral-float normalization (Task 1), harvest no-match (Task 2), harvest append-not-replace (Task 2) — each owned by a named test.
- **Proportion:** plan is signatures + test names/assertions; no Go bodies transcribed — the Go source travels as the spec and is cited by function name.
