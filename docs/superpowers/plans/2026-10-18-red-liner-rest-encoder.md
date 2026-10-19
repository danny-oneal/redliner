# Red Liner — REST Encoder Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. REQUIRED during coding: the `rust-skills` skill (idiomatic ownership, error handling, trait objects).

**Goal:** Add a REST request encoder to Red Liner behind the existing `Targeter` seam, so `red-liner -config rest.yaml` fires a paced REST load (per-scenario method/path/body/headers) with the same pacing, auth, weighted-mix, SLO gate, and exit codes already proven for GraphQL.

**Architecture:** Port Go's `encoder` interface + `newEncoder` factory + single `NewTargeter` (currently collapsed into a GraphQL-only `GraphqlTargeter`). Introduce an `Encoder` trait (`encode` + `operation`), make one protocol-agnostic `MixTargeter` that owns the rng/mix/lock and delegates to a `Box<dyn Encoder>`, and add a `new_encoder(&Config)` factory that selects GraphQL or REST on `target.protocol`. `RestEncoder` precompiles per-scenario merged headers and a path→regex matcher (placeholders become wildcard segments) exactly as Go `restEncoder`/`pathPattern`. Nothing upstream (pacing, attacker, metrics, SLO, `run`) changes — `run` already drives an `Arc<dyn Targeter>`.

**Tech Stack:** Rust (edition 2021, MSRV 1.88 per workspace). New dependency: `regex` (path-pattern matcher). Existing: `http` (Method/HeaderMap), `serde_json`, `rand`/`rand_chacha`, `thiserror`.

**Spec:** The Go implementation is the behavioral authority — `protocol.go` (`restEncoder`, `restRoute`, `pathPattern`, `newRESTEncoder`, `newEncoder`) and `targeter.go` (`NewTargeter`). This plan is the next step named in `docs/superpowers/plans/2026-10-17-red-liner-rust-plumbing.md` → "Deferred to later plans" → "REST encoder … behind the same `Targeter` seam — the immediate next plan."

## Global Constraints

- Config parsing, defaults, and validation for REST scenarios already exist and are **not** re-implemented here: `config/mod.rs` defaults empty `request.method` → `"GET"`, requires `request.path` for REST, and rejects GraphQL fields under a REST protocol. Treat `Scenario`/`RequestConfig`/`TargetConfig` as given.
- `volley` MUST NOT gain any dependency from this work — all REST code lives in the `red-liner` binary's `encode` module.
- Exit-code contract unchanged: `0` pass, `1` SLO violated, `2` config/runtime error.
- Seeded runs stay reproducible: the single `Mutex<ChaCha8Rng>` seeded from `load.seed` lives in `MixTargeter`, shared across protocols.
- Base headers, auth-from-env, and `WeightedMix` are reused verbatim — do not duplicate `base_headers` or mix logic.
- `new regex` dep: pin an exact version at least two weeks old (`regex = "1.11.1"`).

## Review Focus

Inputs the Go REST spec implies that the GraphQL tests never exercised; each is pinned to Task 2.

- **Scenario header overrides a base header** → per-scenario `request.headers` must override the shared base map for that route only, without mutating the base or other routes (Go clones then `Set`). (Task 2)
- **Body present, no explicit `Content-Type`** → encoder adds `application/json`; a user-supplied `Content-Type` is left untouched; GET-with-no-body sends no body. (Task 2)
- **`operation()` on a concrete URL** → a fired URL whose path filled a `{{placeholder}}` segment matches its originating scenario by method+pattern; a wrong-method or non-matching URL returns `None`. (Task 2)
- **Base URL trailing slash + path join** → `url: http://h/` + `path: /x` yields `http://h/x`, not `http://h//x` (Go `TrimRight(url,"/")`). (Task 2)
- **Method case** → config lowercase `get`/`post` encodes as `GET`/`POST` (Go `ToUpper`). (Task 2)

---

## File Structure

```
crates/red-liner/
  Cargo.toml              + regex dependency (Task 2)
  src/encode/mod.rs       + Encoder trait, MixTargeter (replaces GraphqlTargeter), new_encoder factory
  src/encode/graphql.rs   GraphqlEncoder now impls Encoder (operation gains a method arg it ignores)
  src/encode/rest.rs      NEW: RestEncoder, RestRoute, path_pattern  (Task 2)
  src/run.rs              builds the targeter via new_encoder + MixTargeter (Task 1, then REST-agnostic)
  src/lib.rs              re-export adjustments if GraphqlTargeter was re-exported
  tests/e2e.rs            + REST end-to-end test + request-line-capturing mock (Task 2)
```

---

## Task 1: `Encoder` trait, generic `MixTargeter`, GraphQL onto the seam

Pure refactor — no REST yet. Collapses the GraphQL-only targeter into a protocol-agnostic one so Task 2 drops REST in without touching `run`. Deliverable: the whole workspace builds and every existing test passes through the new seam.

**Files:**
- Modify: `crates/red-liner/src/encode/mod.rs`, `crates/red-liner/src/encode/graphql.rs`, `crates/red-liner/src/run.rs`, `crates/red-liner/src/lib.rs` (only if it re-exports `GraphqlTargeter`)
- Test: existing inline tests in `encode/mod.rs` and `encode/graphql.rs` (updated), existing `tests/e2e.rs` (unchanged)

**Interfaces:**
- Consumes: `GraphqlEncoder` (unchanged constructor `GraphqlEncoder::new(&TargetConfig)`), `WeightedMix`, `volley::{Target, Targeter}`.
- Produces:
  - `pub trait Encoder: Send + Sync { fn encode(&self, t: &mut Target, sc: &Scenario) -> Result<(), EncodeError>; fn operation(&self, method: &str, url: &str) -> Option<String>; }`
  - `pub struct MixTargeter { enc: Box<dyn Encoder>, mix: WeightedMix, rng: Mutex<ChaCha8Rng> }`; `pub fn new(enc: Box<dyn Encoder>, mix: WeightedMix, seed: u64) -> Self`; `impl volley::Targeter for MixTargeter` (locks rng, `mix.pick`, `enc.encode`, maps `EncodeError` → `volley::Error::Build`). This is the old `GraphqlTargeter` body with the field typed as the trait object.
  - `impl Encoder for GraphqlEncoder` — `encode` delegates to the existing inherent method; `operation(&self, _method, url)` delegates to the existing inherent `operation(url)` (method ignored for GraphQL).
  - `GraphqlTargeter` is removed (replaced by `MixTargeter`).

- [ ] **Step 1: Update the failing tests first** — in `encode/mod.rs`, rename the `GraphqlTargeter` usages in `targeter_is_reproducible_from_seed` to build `MixTargeter::new(Box::new(GraphqlEncoder::new(&cfg).unwrap()), WeightedMix::new(...), 7)`. In `encode/graphql.rs`, change the two `e.operation(&t.url)` / `e.operation("http://h/graphql")` calls to pass a method arg: `Encoder::operation(&e, "POST", &t.url)` and `Encoder::operation(&e, "POST", "http://h/graphql")`.
- [ ] **Step 2: Run to verify they fail** — `cargo test -p red-liner` → FAIL to compile (`MixTargeter`/trait not defined).
- [ ] **Step 3: Implement the trait, `MixTargeter`, and the `GraphqlEncoder` impl.** Move the `GraphqlTargeter` struct/impl to `MixTargeter` with `enc: Box<dyn Encoder>`. Add the `Encoder` trait. Add `impl Encoder for GraphqlEncoder` delegating to the existing inherent `encode`/`operation` (keep the inherent methods so the graphql inline tests that call them directly still compile, or route those through the trait — either is fine as long as both compile). Update `run.rs`: replace `use crate::encode::{GraphqlEncoder, GraphqlTargeter};` and the construction with `let enc: Box<dyn Encoder> = Box::new(GraphqlEncoder::new(&cfg.target)?); let targeter = Arc::new(MixTargeter::new(enc, mix, cfg.load.seed as u64));`. Fix `lib.rs` re-exports if needed.
- [ ] **Step 4: Run to verify they pass** — `cargo test --workspace` → PASS (all prior tests green through the new seam).
- [ ] **Step 5: Commit** — `refactor(red-liner): Encoder trait + generic MixTargeter over the protocol seam`.

---

## Task 2: `RestEncoder` — encode, path-pattern `operation`, protocol factory, REST e2e

Adds the REST half and the `new_encoder` factory, then switches `run` to the factory so protocol selection is config-driven. Deliverable: a REST config fires a real paced REST load end-to-end and exits on the SLO gate.

**Files:**
- Create: `crates/red-liner/src/encode/rest.rs`
- Modify: `crates/red-liner/src/encode/mod.rs` (add `pub mod rest;`, `pub use rest::RestEncoder;`, `new_encoder` factory), `crates/red-liner/src/run.rs` (use the factory), `crates/red-liner/Cargo.toml` (+`regex`)
- Test: inline unit tests in `encode/rest.rs`; one REST case + a request-line mock in `crates/red-liner/tests/e2e.rs`
- Reference: Go `protocol.go` `restEncoder`/`restRoute`/`pathPattern`/`newRESTEncoder`/`newEncoder`.

**Interfaces:**
- Consumes: `Encoder`, `EncodeError`, `base_headers` (Task 1 / existing); `Config`, `Scenario`, `TargetConfig`, `Protocol` (config); `volley::Target`; `regex::Regex`.
- Produces:
  - `src/encode/rest.rs`:
    - `struct RestRoute { name: String, method: String, headers: http::HeaderMap, pattern: regex::Regex }` (private).
    - `pub struct RestEncoder { base_url: String, routes: Vec<RestRoute> }`.
    - `pub fn new(cfg: &TargetConfig, scenarios: &[Scenario]) -> Result<RestEncoder, EncodeError>` — `base_url` = `cfg.url` with trailing `/` trimmed; `base` = `base_headers(cfg)?`; per scenario: clone `base`, apply `request.headers` (override), and if `request.body` non-empty and no `content-type` present insert `application/json`; `method` = `request.method.to_ascii_uppercase()`; `pattern` = `path_pattern(&base_url, &request.path)`.
    - `impl Encoder for RestEncoder`: `encode(t, sc)` finds the route by `sc.name`, sets `t.method` (parse via `http::Method::from_bytes(route.method.as_bytes())`, map error → `EncodeError::InvalidHeader`/a suitable variant), `t.url = format!("{}{}", self.base_url, sc.request.path)`, `t.header = route.headers.clone()`, and `t.body = sc.request.body.clone().into_bytes()` only when non-empty. `// SEAM: {{placeholder}} pool templating will render path/body in the data-pools plan; static pass uses the raw strings.` `operation(method, url)` returns the first route whose `method == method` and `pattern.is_match(url)`, else `None`.
    - `fn path_pattern(base: &str, path: &str) -> regex::Regex` (see Step 3 for the construction).
  - `src/encode/mod.rs`: `pub fn new_encoder(cfg: &Config) -> Result<Box<dyn Encoder>, EncodeError>` matching `cfg.target.protocol` → `Protocol::Graphql => Box::new(GraphqlEncoder::new(&cfg.target)?)`, `Protocol::Rest => Box::new(RestEncoder::new(&cfg.target, &cfg.scenarios)?)`.
  - `src/run.rs`: replace the hand-built GraphQL encoder with `let enc = new_encoder(&cfg)?;` (map `EncodeError` into the `anyhow` path).

- [ ] **Step 1: Write the failing tests.**
  - In `encode/rest.rs`:
    - `rest_encode_builds_request`: scenario `{ name: "GetProduct", method: "get", path: "/products/5", headers: {X-Trace: t1} }`, `TargetConfig { url: "http://h/", .. }`. After `encode`: `t.method == Method::GET`, `t.url == "http://h/products/5"` (no double slash), `t.header["x-trace"] == "t1"`, `t.header["accept"] == "application/json"`, `t.body` empty. *(Review Focus: trailing slash, method case)*
    - `rest_body_sets_json_content_type`: POST scenario with `body: "{\"a\":1}"` and no `Content-Type` → `t.header["content-type"] == "application/json"` and `t.body` == the bytes; a second scenario that sets `Content-Type: text/plain` keeps `text/plain`. *(Review Focus: body/content-type)*
    - `rest_headers_do_not_leak_between_routes`: two scenarios, only the first sets `X-A` → encoding the second yields no `x-a` header, and the base map is unchanged. *(Review Focus: header override isolation)*
    - `rest_operation_matches_by_pattern`: encoder over scenario `GetProduct` path `/products/{{id}}`; `operation("GET", "http://h/products/42")` → `Some("GetProduct")`; `operation("POST", "http://h/products/42")` → `None`; `operation("GET", "http://h/orders/1")` → `None`. *(Review Focus: operation matching)*
  - In `tests/e2e.rs`: `e2e_rest_runs_and_exits_zero` — a mock that records the first request line (`"GET /ping HTTP/1.1"`), a temp config with `target.protocol: rest` and scenario `{ name: ping, request: { method: GET, path: /ping } }`, `constant 50 rps` for `1s`, no SLOs → `run` returns `Ok`, hits > 10, and a recorded line starts with `GET /ping`.
- [ ] **Step 2: Run to verify they fail** — `cargo test -p red-liner rest` and `cargo test -p red-liner --test e2e e2e_rest` → FAIL (module/`regex` missing).
- [ ] **Step 3: Implement.** Add `regex = "1.11.1"` to `crates/red-liner/Cargo.toml`. Implement `RestEncoder`, the `Encoder` impl, and `path_pattern`, then the `new_encoder` factory and the `run.rs` switch. `path_pattern` ports Go `pathPattern`: scan `base+path` for `{{`…`}}` groups, emitting `regex::escape(literal)` for the fixed parts and `[^/?&]+` for each placeholder, anchored `^…$`:

```rust
fn path_pattern(base: &str, path: &str) -> regex::Regex {
    let mut pat = String::from("^");
    let mut rest = format!("{base}{path}");
    while let Some(i) = rest.find("{{") {
        pat.push_str(&regex::escape(&rest[..i]));
        pat.push_str("[^/?&]+");
        match rest[i..].find("}}") {
            Some(j) => rest = rest[i + j + 2..].to_string(),
            None => { rest.clear(); break; }
        }
    }
    pat.push_str(&regex::escape(&rest));
    pat.push('$');
    regex::Regex::new(&pat).expect("path pattern is a valid regex")
}
```

  For the e2e mock, extend/clone the existing hand-rolled listener to capture the request line (`data` up to the first `\r\n`) into a shared `Vec<String>`; respond `200 {"data":{}}` as today.
- [ ] **Step 4: Run to verify they pass** — `cargo test --workspace` → PASS (GraphQL e2e still green; REST e2e green).
- [ ] **Step 5: Commit** — `feat(red-liner): REST encoder with path-pattern operation matching and protocol factory`.

---

## Self-Review notes

- **Spec coverage:** `restEncoder.encode` → Task 2 `encode` + `rest_encode_builds_request`/`rest_body_sets_json_content_type`; `restRoute` header merge → `rest_headers_do_not_leak_between_routes`; `pathPattern` + `restEncoder.operation` → `path_pattern` + `rest_operation_matches_by_pattern`; `newRESTEncoder` → `RestEncoder::new`; `newEncoder` factory → `new_encoder`; `NewTargeter` (protocol-agnostic) → Task 1 `MixTargeter`. Config/validation for REST already shipped in the prior plan — intentionally not re-done.
- **Step scan:** every code step gives a signature + the file; the one non-obvious algorithm (`path_pattern`) carries its body because the signature does not determine it; test steps give names + assertions with exact values.
- **Type consistency:** `Encoder`, `MixTargeter`, `RestEncoder`, `new_encoder`, `operation(method, url)` are named identically across producer (Task 1/2) and consumer (`run.rs`). `MixTargeter::new` takes `Box<dyn Encoder>` in both tasks.
- **Review Focus:** all five lines are owned by Task 2 tests (header override, body/content-type, operation matching, trailing-slash join, method case).
- **Proportion:** plan is signatures + test assertions; the sole transcribed body is `path_pattern`, a direct port.
