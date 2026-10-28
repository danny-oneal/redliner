# Production Blockers B1–B3 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close the three 0.4.0 readiness blockers: credentials leaking into `run_manifest.json` (B1), integer-truncated rates that silently fire nothing (B2), and runs where nothing succeeded exiting 0 (B3).

**Architecture:** B1 adds a `RedactedConfig` newtype as the manifest's `config` field type, so the only way to put a config on disk is through redaction. B3 adds a built-in `successful_requests` check in `slo.rs` that `run_inner` appends (and fails on) regardless of SLO config, following the `replay_delivery` precedent. B2 adds `ConstantPacer::from_rate(f64)` in `volley` (micro-rps resolution), drops the ramp `trunc()`, and tightens load validation so a profile that cannot fire one request is a config error.

**Tech Stack:** Rust 1.88 / edition 2021, serde, tokio, `volley` engine crate.

**Spec:** `PRODUCTION-READINESS-DO-NOT-COMMIT.md` (repo root, untracked), sections B1, B2, B3. The relevant facts are restated per task, so the plan stands alone if that file is gone.

## Global Constraints

- Do not run `cargo fmt`; match the surrounding hand formatting.
- `make lint` (clippy `-D warnings`) and `make test` must pass after every task; MSRV 1.88 (`cargo +1.88 check`).
- `volley` stays protocol- and config-agnostic: it may gain an f64-rate constructor and a metrics getter, nothing that knows about YAML or redliner.
- No `unwrap()` outside tests. Error/warning messages lowercase.
- Test first: each task starts with a failing test.
- Every user-visible change gets a line in `CHANGELOG.md` under `## [Unreleased]` (`### Security`, `### Changed`, `### Fixed` as noted per task).
- Conventional Commits; work on branch `fix/production-blockers` (create from `main` before Task 1).

## Review Focus

1. **Header-name casing and vendor names** (`AUTHORIZATION`, `X-Api-Key`, `x-amz-security-token`) must be redacted; ordinary headers (`Accept`, `Content-Type`) must survive for reproducibility. → Task 1, `redacts_sensitive_headers_case_insensitively`.
2. **An interrupted run that got no successes** must still exit `130`, not `1`: the user stopped it. → Task 2, `e2e_interrupted_dead_target_exits_130`.
3. **A target that answers but every answer fails checks** (GraphQL 200 + `errors`) counts as zero successes, not as "it responded". → Task 2, `e2e_all_checks_failed_without_slos_exits_one`.
4. **Existing integral-rate configs must produce the identical schedule** after the pacer change, so seeded runs and timing-sensitive tests do not shift. → Task 3, `from_rate_integral_matches_legacy_schedule`.
5. **`rate: .nan` / `.inf` in YAML** pass today's `rate <= 0.0` check (NaN compares false) and become 0 or `u64::MAX` hits/s; both must be validation errors. → Task 4, `non_finite_rates_are_rejected`.

---

### Task 1: B1 — redact credential headers in `run_manifest.json`

Today `RunManifest.config: Config` (`crates/redliner/src/report.rs:285`) serialises every header verbatim; `run.rs:211` fills it with `cfg.clone()`. `-H` flags merge into `target.headers` (`cli.rs:128`), so `-H 'Authorization: Bearer x'` lands on disk. Both the pre-traffic and the final manifest use this struct.

**Files:**
- Modify: `crates/redliner/src/report.rs` (`RunManifest`, new `RedactedConfig`, tests module at `:479`)
- Modify: `crates/redliner/src/run.rs:207-218` (manifest construction)
- Test: `crates/redliner/src/report.rs` (unit), `crates/redliner/tests/e2e.rs`
- Docs: `docs/configuration.md` (`target.headers` row at `:35`, REST `headers` row at `:335`), `README.md:281`, `CHANGELOG.md` (`### Security`)

**Interfaces:**
- Produces: `pub struct RedactedConfig(Config)` with `#[derive(Serialize)] #[serde(transparent)]` and `impl From<&Config> for RedactedConfig`; `pub const REDACTED: &str = "<redacted>";` `RunManifest.config: RedactedConfig`.

- [ ] **Step 1: Write the failing unit tests** in `report.rs` `mod tests`:

```rust
#[test]
fn redacts_sensitive_headers_case_insensitively() {
    let mut cfg = Config::default();
    for (k, v) in [
        ("AUTHORIZATION", "Bearer s1"), ("Proxy-Authorization", "s2"), ("Cookie", "s3"),
        ("X-Api-Key", "s4"), ("x-amz-security-token", "s5"), ("X-Client-Secret", "s6"),
        ("X-Session-Id", "s7"), ("Accept", "application/json"), ("Content-Type", "text/plain"),
    ] {
        cfg.target.headers.insert(k.into(), v.into());
    }
    let v = serde_json::to_value(RedactedConfig::from(&cfg)).unwrap();
    let h = &v["target"]["headers"];
    for k in ["AUTHORIZATION", "Proxy-Authorization", "Cookie", "X-Api-Key",
              "x-amz-security-token", "X-Client-Secret", "X-Session-Id"] {
        assert_eq!(h[k], REDACTED, "{k}");
    }
    assert_eq!(h["Accept"], "application/json");
    assert_eq!(h["Content-Type"], "text/plain");
    // the source config still carries the real value (requests use it)
    assert_eq!(cfg.target.headers["AUTHORIZATION"], "Bearer s1");
}

#[test]
fn redacts_scenario_and_harvest_headers() {
    // one scenario and one harvest entry, each with `Authorization: tok`
    // and `Accept: a`; assert scenarios[0].request.headers.Authorization and
    // data.harvest[0].request.headers.Authorization == REDACTED, Accept == "a",
    // and the serialised JSON string does not contain "tok".
}
```

- [ ] **Step 2: Write the failing e2e test** `e2e_manifest_redacts_header_credentials` in `tests/e2e.rs`, modelled on `e2e_zero_config_url_benchmarks_one_endpoint`: `--url <origin>/items --rate 20 --duration 1s --live-stats 0s --out <dir> -H "Authorization: Bearer SUPERSECRET" -H "X-Api-Key: k123" -H "Accept: application/json"`. Assert `Exit::Ok`; the raw text of `newest_report(&dir).join("run_manifest.json")` contains neither `SUPERSECRET` nor `k123`; `manifest["config"]["target"]["headers"]["Authorization"] == "<redacted>"`; `["Accept"] == "application/json"`.

- [ ] **Step 3: Run to verify they fail**

Run: `cargo test -p redliner --lib report::tests::redacts && cargo test -p redliner --test e2e e2e_manifest_redacts_header_credentials`
Expected: compile error (`RedactedConfig` undefined), then after a stub, e2e FAIL (manifest contains `SUPERSECRET`).

- [ ] **Step 4: Implement `RedactedConfig` in `report.rs`; change `RunManifest.config` to it; in `run.rs` set `config: RedactedConfig::from(&cfg)`.**

`From<&Config>` clones the config and replaces values in `target.headers`, every `scenarios[i].request.headers`, and every `data.harvest[i].request.headers`. A header is sensitive when its lowercased name contains any of: `auth`, `cookie`, `key`, `token`, `secret`, `passw`, `session`, `credential`, `signature` (a private `const SENSITIVE_HEADER_PARTS: [&str; 9]` and `fn is_sensitive_header(name: &str) -> bool`). The whole value is replaced, placeholders included. Doc comment on the type says what is redacted and that URLs, bodies and GraphQL variables are not.

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p redliner --lib report::tests && cargo test -p redliner --test e2e manifest`
Expected: PASS. Then `make lint`.

- [ ] **Step 6: Docs and changelog.** `docs/configuration.md`: after the `target` table, one paragraph: "`run_manifest.json` records the config with the value of any header whose name contains `auth`, `cookie`, `key`, `token`, `secret`, `passw`, `session`, `credential` or `signature` (case-insensitive) replaced by `<redacted>`; this covers `target.headers`, `-H`, scenario and harvest headers. Values in URLs, bodies and variables are written as-is: keep credentials in headers or `target.auth`." `README.md:281`: "full config (credential headers redacted), seed, and SLO verdicts". `CHANGELOG.md` `### Security`: "`run_manifest.json` no longer stores credential header values (`Authorization`, `Cookie`, `*key*`, `*token*`, `*secret*`, …) from `target.headers`, `-H`, or scenario/harvest headers; they are written as `<redacted>`."

- [ ] **Step 7: Commit**

```bash
git add crates/redliner/src/report.rs crates/redliner/src/run.rs crates/redliner/tests/e2e.rs docs/configuration.md README.md CHANGELOG.md
git commit -m "fix(report): redact credential header values in run_manifest.json"
```

---

### Task 2: B3 — fail a run in which no request succeeded

`run.rs:315` returns `Exit::Ok` unless SLOs are enforced, so a run where 100% of requests were connection-refused (or failed checks) passes CI. Fix: a built-in check that fails when `requests == 0` or zero requests succeeded, applied whether or not SLOs are configured, skipped on interrupt. "Succeeded" is volley's definition after the checker ran: no error and status 200–399.

**Files:**
- Modify: `crates/volley/src/metrics.rs` (getter), `crates/redliner/src/slo.rs`, `crates/redliner/src/run.rs:256-315`
- Test: `crates/volley/src/metrics.rs`, `crates/redliner/src/slo.rs` (`mod tests` at `:63`), `crates/redliner/tests/e2e.rs`
- Docs: `README.md:118` (exit codes), `docs/configuration.md` `## slos`, `CHANGELOG.md` (`### Changed`)

**Interfaces:**
- Produces: `volley::Metrics::successes(&self) -> u64`; `slo::no_success(m: &Metrics) -> Option<SloResult>` returning a failed result `{ name: "successful_requests", target: ">= 1", observed: "no requests fired" | "0 of {n}", passed: false }`, or `None` when at least one request succeeded.

- [ ] **Step 1: Write the failing unit tests.**

`metrics.rs`: `successes_counts_ok_results` — add 3 results with code 200 and 1 with code 500, `close()`, assert `m.successes() == 3`.

`slo.rs`:
```rust
#[test]
fn no_success_flags_empty_and_all_failed_runs() {
    let mut empty = Metrics::new();
    empty.close();
    let r = no_success(&empty).expect("empty run must fail");
    assert_eq!((r.name.as_str(), r.observed.as_str(), r.passed),
               ("successful_requests", "no requests fired", false));
    // 20 results with code 0 and an error -> observed "0 of 20"
    // one extra result with code 200 -> no_success(..) is None
}
```

- [ ] **Step 2: Write the failing e2e tests** in `tests/e2e.rs`:
  - `e2e_dead_target_without_slos_exits_one`: `--url http://127.0.0.1:1/ --rate 20 --duration 1s --live-stats 0s --out <dir>` → `Exit::SloFailed`; manifest `passed == false`; `slo_results` contains an entry with `name == "successful_requests"` and `observed == "0 of N"` for some N > 0.
  - `e2e_all_checks_failed_without_slos_exits_one`: `spawn_error_mock()` + `write_cfg(&dir, &mock.url, "")` (no SLOs) → `Exit::SloFailed`.
  - `e2e_interrupted_dead_target_exits_130`: same args as the first, `--duration 30s`, through `run_with_interrupt` with a 500 ms sleep (as in `e2e_interrupt_writes_partial_report_and_exits_130`) → `Exit::Interrupted`; manifest `slo_results` has no `successful_requests` entry.

- [ ] **Step 3: Run to verify they fail**

Run: `cargo test -p redliner --lib slo && cargo test -p redliner --test e2e without_slos_exits_one`
Expected: compile error for `no_success`; e2e FAIL with `left: Ok, right: SloFailed`.

- [ ] **Step 4: Implement.** Add `Metrics::successes`. Add `slo::no_success`. In `run_inner`, after the replay-delivery block and only when `!interrupted`: if `no_success(collector.overall())` is `Some(r)`, print `warning: no request succeeded ({observed}; status: {report::status_classes(..)}); is the target up?` to stderr, push `r` into `results`, set `passed = false` and a `none_succeeded` flag; the exit condition becomes `fell_behind || none_succeeded || (cfg.slos.enforced() && !passed)`. The result reaches the summary, the redline banner, `junit.xml` and the manifest through the existing `results` plumbing. Only a failing check is added: passing runs' output is unchanged.

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p volley && cargo test -p redliner`
Expected: PASS, including every pre-existing e2e test (all of them hit a live mock, so none should flip). Then `make lint`.

- [ ] **Step 6: Docs and changelog.** `README.md:118` exit codes: "`1` an SLO was violated, a replay fell behind, or no request succeeded". `docs/configuration.md` `## slos`, new paragraph: "Independent of these thresholds, a run in which no request succeeded (none fired, or every one failed at the transport layer or a check) fails the built-in `successful_requests` check, listed with the SLOs and in `junit.xml`, and exits `1`. An interrupted run exits `130` instead." `CHANGELOG.md` `### Changed`: "A run in which no request succeeded now exits `1` (built-in `successful_requests` check) even with no SLOs configured; previously a down target passed."

- [ ] **Step 7: Commit**

```bash
git add crates/volley/src/metrics.rs crates/redliner/src/slo.rs crates/redliner/src/run.rs crates/redliner/tests/e2e.rs README.md docs/configuration.md CHANGELOG.md
git commit -m "fix(run): exit 1 when no request succeeded, even without SLOs"
```

---

### Task 3: B2 — fractional rates for `constant`, `steps` and `ramp`

`phases.rs:20` builds `ConstantPacer { freq: rate as u64, per: 1s }` (0.5 → 0 → stops at once; 2.7 → 2), and `:46` passes `from.trunc()` to `LinearPacer` (which already takes f64). Fix by expressing a rate as micro-hits per 10⁶ s: for an integral rate the ratio `freq/per` is unchanged, so `ConstantPacer::pace`'s integer arithmetic yields exactly the same schedule as before.

**Files:**
- Modify: `crates/volley/src/pacer.rs` (`impl ConstantPacer`), `crates/redliner/src/phases.rs:19-21,46`
- Test: `crates/volley/src/pacer.rs` `mod tests`, `crates/redliner/src/phases.rs` `mod tests`, `crates/redliner/tests/e2e.rs`
- Docs: `docs/configuration.md:66` (`rate` row), `CHANGELOG.md` (`### Fixed`)

**Interfaces:**
- Produces: `impl ConstantPacer { pub fn from_rate(rate: f64) -> Self }` — `freq = (rate * 1e6).round() as u64`, `per = Duration::from_secs(1_000_000)`; resolution 10⁻⁶ hits/s. NaN/negative give `freq: 0` (the pacer stops at once, as today); validation (Task 4) keeps those out of real runs. Doc comment states both.
- Consumes: nothing new; `phases::per_sec` becomes a call to `ConstantPacer::from_rate` (or is inlined at its two call sites).

- [ ] **Step 1: Write the failing volley tests** in `pacer.rs`:
  - `from_rate_fractional_paces_exactly`: drive the loop from `constant_pacer_targets_rate` for 10 s; `from_rate(2.5)` gives 25 hits, `from_rate(0.5)` gives 5; `(from_rate(2.7).rate(Duration::ZERO) - 2.7).abs() < 1e-9`.
  - `from_rate_integral_matches_legacy_schedule`: for 1000 hits, walking `elapsed` by the returned wait, `from_rate(100.0).pace(e, h) == ConstantPacer { freq: 100, per: Duration::from_secs(1) }.pace(e, h)`; repeat with rate 3.0 (a non-terminating 1/3 s interval).
  - `from_rate_nan_stops`: `from_rate(f64::NAN).pace(Duration::ZERO, 0).1` is `true`.

- [ ] **Step 2: Write the failing phases test** `build_phases_keeps_fractional_rates`: `constant` rate 2.5 → `p[0].pacer.rate(Duration::ZERO) == 2.5`; `steps` `[step 0.5 rps 4 s]` → 0.5; `ramp { from: 0.5, to: 2.5 }` over 10 s → `p[0].pacer.rate(Duration::ZERO) == 0.5` (currently 0.0). Compare with `(a - b).abs() < 1e-9`.

- [ ] **Step 3: Write the failing e2e test** `e2e_fractional_constant_rate_fires`: `write_cfg`-style config with `profile: constant`, `rate: 2.5`, `duration: 2s` against `spawn_mock()` → `Exit::Ok` and `mock.hits == 4` (hits due at 0.4/0.8/1.2/1.6 s; the 2.0 s hit falls on the deadline and is not fired; truncation to 2 rps gave 3).

- [ ] **Step 4: Run to verify they fail**

Run: `cargo test -p volley from_rate && cargo test -p redliner --lib phases && cargo test -p redliner --test e2e fractional`
Expected: compile error for `from_rate`; phases ramp assertion `0.0 != 0.5`; e2e `3 != 4`.

- [ ] **Step 5: Implement** `ConstantPacer::from_rate`; use it for `constant` and `steps` in `phases.rs`; pass `load.ramp.from` (no `trunc()`) to `LinearPacer`. Leave the warmup pacer in `run.rs` alone (it already clamps to ≥ 1 rps).

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo test --workspace`
Expected: PASS. Then `make lint`.

- [ ] **Step 7: Docs and changelog.** `docs/configuration.md:66` `rate` notes: "requests per second (`constant`, `poisson`); fractional rates such as `0.5` or `2.7` are honoured". `CHANGELOG.md` `### Fixed`: "`constant`, `steps` and `ramp` no longer truncate rates to whole numbers: `rate: 0.5` fired nothing and `rate: 2.7` ran at 2 rps."

- [ ] **Step 8: Commit**

```bash
git add crates/volley/src/pacer.rs crates/redliner/src/phases.rs crates/redliner/tests/e2e.rs docs/configuration.md CHANGELOG.md
git commit -m "fix(volley): honour fractional rates in constant, steps and ramp pacers"
```

---

### Task 4: B2 — reject load settings that cannot fire a request

With Task 3, no rate truncates, but a run can still be configured to fire nothing (`rate: 0.5, duration: 1s`), and NaN slips past `rate <= 0.0`. Validation (`config/mod.rs:495-520`) should refuse both. Constant pacing puts the first hit at `1/rate` and the attacker never fires a hit due at the deadline, so `constant` and each `steps` entry need `rate × secs > 1`; `poisson` needs `rate × secs >= 1`; `ramp` needs `(from + to) / 2 × secs >= 1`. Task 2 is the runtime backstop for anything that still slips through (e.g. an unlucky Poisson draw).

**Files:**
- Modify: `crates/redliner/src/config/mod.rs:495-520`
- Test: `crates/redliner/src/config/tests.rs`
- Docs: `docs/configuration.md` `## load` (below the table), `CHANGELOG.md` (`### Fixed`, same bullet list as Task 3)

**Interfaces:**
- Consumes/produces nothing outside `config`. Keep the existing message substrings `load.rate must be > 0` and `load.steps[{i}]: rate and duration must be > 0` (asserted at `tests.rs:245`); append "and finite" rather than rewording.

- [ ] **Step 1: Write the failing tests** in `config/tests.rs` (use `load_str` + `err_text`, GraphQL head `GQL_HEAD` + `ONE_GQL`):
  - `non_finite_rates_are_rejected`: `profile: constant, rate: .nan` and `rate: .inf` → error contains `load.rate must be > 0 and finite`; `steps: [{rate: .nan, duration: 1s}]` → contains `load.steps[0]: rate and duration must be > 0 and finite`; `ramp: {from: .nan, to: 10}` → contains `load.ramp`.
  - `load_that_fires_nothing_is_rejected`: `constant, rate: 0.5, duration: 1s` → contains `fires no requests`; `constant, rate: 1, duration: 1s` → same; `poisson, rate: 0.5, duration: 1s` → same; `steps: [{rate: 10, duration: 1s}, {rate: 0.5, duration: 1s}]` → contains `load.steps[1]` and `fires no requests`; `ramp {from: 0, to: 0.5}, duration: 1s` → same.
  - `fractional_rates_that_fire_are_accepted`: `constant, rate: 0.5, duration: 3s`, `poisson, rate: 0.5, duration: 2s`, `ramp {from: 0.5, to: 2.5}, duration: 10s` all load `Ok`.

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p redliner --lib config::tests`
Expected: the three new tests FAIL (NaN accepted; `rate: 0.5, duration: 1s` accepted).

- [ ] **Step 3: Implement** in `validate`'s profile match. Rate checks become finite-and-positive (a small `fn positive_finite(x: f64) -> bool`; avoid `!(a <= b)` forms, which trip clippy's `neg_cmp_op_on_partial_ord`). Ramp requires both ends finite, `from >= 0`, `to > 0`. Then the request-count check, message: `load.rate × load.duration fires no requests for profile {profile:?} ({rate} rps for {dur:?}); raise either` (for a step: `load.steps[{i}]: {rate} rps for {dur:?} fires no requests`; for ramp: `load.ramp averages {avg} rps over {dur:?}, which fires no requests`). Skip the count check when the rate check already failed, so one mistake gives one message.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --workspace && make lint && make validate CONFIG=configs/rest-example.yaml`
Expected: PASS; every config under `configs/` still validates (`for f in configs/*.yaml; do make validate CONFIG=$f; done`).

- [ ] **Step 5: Docs and changelog.** `docs/configuration.md` `## load`, after the table: "A load that cannot fire a single request is rejected: `constant` and each `steps` entry need `rate × duration > 1` (the first hit is due at `1/rate`, and a hit due exactly at the end is not sent), `poisson` needs `rate × duration ≥ 1`, and `ramp` needs `(from + to) / 2 × duration ≥ 1`. Rates must be finite." `CHANGELOG.md` `### Fixed`: "Validation rejects non-finite rates (`.nan`, `.inf`) and loads that would fire no requests."

- [ ] **Step 6: Commit**

```bash
git add crates/redliner/src/config/mod.rs crates/redliner/src/config/tests.rs docs/configuration.md CHANGELOG.md
git commit -m "fix(config): reject non-finite rates and loads that fire no requests"
```

---

### Final verification (after Task 4)

- [ ] `make lint && make test && cargo +1.88 check --workspace && cargo package --workspace --locked --allow-dirty`
- [ ] Re-run the review's three repros against a release build (`cargo build --release`): `-H "Authorization: Bearer SUPERSECRET"` leaves no `SUPERSECRET` in `reports/latest/run_manifest.json`; `profile: constant, rate: 0.5, duration: 3s` fires 1 request; `rate: 0.5, duration: 1s` exits 2 with `fires no requests`; `--url http://127.0.0.1:1/ --rate 20 --duration 1s` exits 1 with the `successful_requests` FAIL line.
