# Production Log Replay Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. REQUIRED during coding: the `rust-skills` skill.

**Goal:** Let `redliner` ingest a structured production log (JSONL or HAR) and fire a load test that reproduces the log's operation order and inter-arrival pace, with `speed` and `copies` amplification, gated by the existing checks/SLOs/reports.

**Architecture:** A new `replay` module parses the log into time-ordered events, classifies each against per-scenario `match` rules, and builds a `Schedule` of `(offset, scenario)` hits (speed and copies are pure transforms of that list). `run.rs` feeds the schedule to the existing `volley::Attacker` through a `SchedulePacer` (waits until each hit's offset) and a `ScheduleTargeter` (encodes the scheduled scenario for hit `seq` using an rng stream keyed by `seq`). The only engine change is passing the hit index to `Targeter::target`.

**Tech Stack:** Rust 2021, MSRV 1.88, tokio, serde/serde_json/serde_yaml_ng, regex (globs), rand_chacha (per-hit rng streams), new: `chrono` (RFC3339 parsing with offsets).

**Spec:** `docs/superpowers/specs/2026-10-21-production-log-replay-design.md` — read its "Refinements made during planning" section; it supersedes the earlier sections where they differ.

## Global Constraints

- `make lint` (clippy `-D warnings`) and `make test` (`cargo test --workspace`) pass at the end of every task.
- `volley` gains no dependencies. Its only change is the `Targeter` signature (Task 1).
- Exit codes are unchanged: `0` pass, `1` SLO failed, `2` config/runtime error. Every replay parse/match/schedule failure is exit `2` and happens **before any request is fired**.
- New dependency, `redliner` only: `chrono = { version = "0.4", default-features = false, features = ["std"] }` (add with `cargo add -p redliner chrono --no-default-features --features std`).
- Every new config struct is `#[serde(deny_unknown_fields, default)]`, like its neighbours in `config/mod.rs`.
- Replay defaults: `speed: 1.0`, `copies: 1`, `on_unmatched: skip`, `format` inferred (`.har` → `har`, anything else → `json`), `fields: {ts: ts, method: method, path: path, operation: operation}`.
- Determinism: the same config + log + `load.seed` produce the same schedule and the same request bytes for every `seq`, regardless of worker concurrency.
- Profile mode (no `replay` block) behaves exactly as before. Existing tests change only mechanically (the new `Targeter` argument, and the new `Scenario`/`RunManifest` fields in struct literals).

## Review Focus

- **Junk lines in a real log** (startup banners, truncated lines, entries without a timestamp): they are skipped and counted as `malformed`, with up to 3 examples in the summary, and the run proceeds. → Task 3 `jsonl_skips_malformed_lines_and_counts_them`.
- **Out-of-order timestamps with mixed offsets** (logs merged from several hosts, `Z` mixed with `+02:00`): events are ordered by true UTC instant, ties keep file order, and no offset is negative. → Task 3 `jsonl_reads_events_in_time_order`.
- **Nothing left to replay** (every event is unmatched, or the window excludes everything): exit `2` with the counts and the top unmatched keys. A zero-request run must never pass the SLO gate. → Task 6 `build_replay_empty_is_error`.
- **Overlapping match rules** (`/skus/*` and `/skus/**`): exit `2` naming both scenarios and the log line, never a silent first match. → Task 4 `overlapping_rules_error_names_both`.
- **`--replay` on a config that still sets `load.profile`**: rejected by validation (exit `2`) instead of being ignored. This only works because CLI overrides now run before validation. → Task 7 `e2e_replay_flag_rejects_profile_config`.

---

## File Structure

```
crates/volley/src/pacer.rs          Targeter::target gains `seq` (Task 1)
crates/volley/src/attacker.rs       passes seq to the targeter (Task 1)
crates/redliner/Cargo.toml          + chrono (Task 2)
crates/redliner/src/config/mod.rs   ReplayConfig, WindowConfig, ReplayFields, MatchConfig,
                                    Config.replay, Scenario.matcher, load_config_file_with (Task 2)
crates/redliner/src/cli.rs          --replay --speed --copies --strict (Task 2)
crates/redliner/src/replay/mod.rs   ReplayError, ReplaySummary, Replay, build_replay (Tasks 3, 6)
crates/redliner/src/replay/parse.rs     RawEvent, Trace, read_trace (Task 3)
crates/redliner/src/replay/matcher.rs   Matcher, Matched, MatchResult, match_events (Task 4)
crates/redliner/src/replay/schedule.rs  ScheduledHit, Schedule, build_schedule (Task 5)
crates/redliner/src/replay/targeter.rs  SchedulePacer, ScheduleTargeter (Task 6)
crates/redliner/src/run.rs          replay branch, --validate output, summary (Task 7)
crates/redliner/src/report.rs       RunManifest.replay (Task 7)
crates/redliner/tests/fixtures/replay/{basic.jsonl,basic.har}   (Task 3)
crates/redliner/tests/e2e.rs        replay end-to-end tests (Task 7)
docs/configuration.md, configs/replay-example.yaml, configs/replay-sample.jsonl,
CHANGELOG.md, README.md             (Task 8)
```

---

### Task 1: Targeter receives the hit index

**Files:**
- Modify: `crates/volley/src/pacer.rs:17-20`
- Modify: `crates/volley/src/attacker.rs:156` (and the test `UrlTargeter` at `:244`)
- Modify: `crates/redliner/src/encode/mod.rs:81-92` (and the test calls at `:127-128`)

**Interfaces:**
- Produces: `fn target(&self, seq: u64, t: &mut Target) -> Result<(), Error>` on `volley::Targeter`. `seq` is the 0-based hit index the attacker already computes (`attacker.rs:121`) and stores in `AttackResult.seq`.

- [ ] **Step 1: Write the failing test** in `attacker.rs` tests

```rust
#[tokio::test]
async fn attack_passes_hit_index_to_targeter() {
    // SeqTargeter { url, seen: Mutex<Vec<u64>> } records `seq` and targets `url`.
    // ConstantPacer 50/s for 300ms against spawn_ok_server().
    let mut seen = targeter.seen.lock().unwrap().clone();
    seen.sort_unstable();
    assert_eq!(seen, (0..results.len() as u64).collect::<Vec<_>>());
    assert!(results.iter().all(|r| seen.contains(&r.seq)));
}
```

- [ ] **Step 2: Run it to make sure it fails**
Run: `cargo test -p volley attack_passes_hit_index_to_targeter`
Expected: compile error E0050 (`target` takes 2 parameters but the trait declares 1).

- [ ] **Step 3: Change the trait and thread `seq` through.** In `run_hit`, call `targeter.target(seq, &mut target)`. `MixTargeter` takes `_seq` and ignores it, so its seeded pick order stays the same. Update the test `UrlTargeter` and the two calls in `encode/mod.rs` tests (pass `0`).

- [ ] **Step 4: Run the workspace tests**
Run: `make test && make lint`
Expected: all pass, no warnings.

- [ ] **Step 5: Commit** — `feat(volley): pass hit index to Targeter::target`

---

### Task 2: Replay config surface and CLI flags

**Files:**
- Modify: `crates/redliner/Cargo.toml` (chrono)
- Modify: `crates/redliner/src/config/mod.rs` (structs, `apply_defaults`, `resolve_files`, `validate`, loader)
- Modify: `crates/redliner/src/cli.rs`, `crates/redliner/src/run.rs:68-69`
- Modify: test `Scenario { .. }` literals in `mix.rs:46`, `encode/mod.rs:100`, `encode/rest.rs:122`, `encode/graphql.rs:102` (add `matcher: None`)
- Test: `crates/redliner/src/config/tests.rs`, `crates/redliner/src/cli.rs` tests

**Interfaces:**
- Produces (all `pub`, in `config`):
  - `ReplayConfig { source: String, format: String, window: Option<WindowConfig>, speed: f64, copies: u64, on_unmatched: String, fields: ReplayFields }`
  - `WindowConfig { start: String, end: String }` (RFC3339; the window is half-open, `[start, end)`)
  - `ReplayFields { ts: String, method: String, path: String, operation: String }`: dot paths into each JSONL object (same syntax as `data::extract_path`)
  - `MatchConfig { operation: String, method: String, path: String }`
  - `Config.replay: Option<ReplayConfig>`; `Scenario.matcher: Option<MatchConfig>` with `#[serde(rename = "match")]`
  - `pub fn load_config_file_with(path: &Path, overrides: impl FnOnce(&mut Config)) -> Result<Config, ConfigError>`: read → parse → **overrides** → defaults → resolve → validate. `load_config_file(p)` becomes `load_config_file_with(p, |_| {})`.
  - `pub(crate) fn parse_rfc3339(s: &str) -> Result<chrono::DateTime<chrono::Utc>, String>`
  - `Args { replay: Option<PathBuf>, speed: Option<f64>, copies: Option<u64>, strict: bool }`

**Rules:**
- `apply_defaults` with `replay: Some`: skip the `load.profile`/`rate`/`duration` defaults. Still default `workers`, `max_workers`, `seed`. Apply the replay defaults from Global Constraints.
- `resolve_files`: a relative `replay.source` joins the config dir, like file pools.
- `apply_overrides`: `--replay p` makes `p` absolute against `std::env::current_dir()` and sets `cfg.replay.get_or_insert_with(Default::default).source`. `--speed`/`--copies` set their fields the same way. `--strict` sets `on_unmatched = "fail"`.
- `run_inner` calls `load_config_file_with(&args.config, |c| args.apply_overrides(c))` and drops the separate `apply_overrides` call.
- `validate` with `replay: Some`. Skip the `load.profile` match and the `weight > 0` check. Push these exact messages:
  - `"load.{field} cannot be combined with replay (replay paces from the log)"` for each of `profile` (non-empty), `rate` (≠0), `duration`, `warmup` (non-zero), `ramp` (`from`/`to` ≠0), `steps` (non-empty)
  - `"replay.source is required"`; `"replay.format {x:?} not supported (json|har)"`; `"replay.speed must be > 0"` (also NaN/inf); `"replay.on_unmatched {x:?} not supported (skip|fail)"`
  - `"replay.window.start/end must both be RFC3339 timestamps"`; `"replay.window.start must be before end"`
  - `"at least one scenario needs a match rule for replay"`
  - graphql: `"scenario {n:?}: match.operation is required for graphql"`; `"scenario {n:?}: match.method/path are REST fields; protocol is graphql"`
  - rest: `"scenario {n:?}: match.path is required for rest and must start with /"`; `"scenario {n:?}: match.operation is a GraphQL field; protocol is rest"`
- In profile mode, `match` blocks are accepted and ignored, so one file can serve both modes.

- [ ] **Step 1: Write the failing tests** in `config/tests.rs` (use `load_str`, `GQL_HEAD`):
  - `replay_defaults_applied`: `replay: {source: log.jsonl}` plus one scenario with `match: {operation: a}` and **no weight** loads OK. Assert speed `1.0`, copies `1`, on_unmatched `"skip"`, format `"json"`, `fields.ts == "ts"`, `load.profile == ""`, and that `source` is absolute and ends with `log.jsonl`.
  - `replay_har_format_inferred`: `source: x.har` gives `format == "har"`.
  - `replay_rejects_load_pacing`: adding `load: {profile: constant, rate: 5}` errors with both the `load.profile` and the `load.rate` messages.
  - `replay_requires_a_match_rule`, `replay_graphql_match_needs_operation`, `replay_rest_match_needs_path`: each checks its exact message.
  - `replay_rejects_bad_knobs`: `speed: -1`, `on_unmatched: maybe`, `window: {start: x, end: y}`, and a window with start after end each produce their message.
  - `profile_mode_ignores_match_blocks`: an ordinary constant-profile config with a `match` block loads OK.
  - In `cli.rs`, `replay_flags_switch_mode`: parse `--replay prod.jsonl --speed 2 --copies 3 --strict` and apply to `Config::default()`. Assert `source == current_dir().join("prod.jsonl")`, speed `2.0`, copies `3`, on_unmatched `"fail"`.

- [ ] **Step 2: Run them to make sure they fail**
Run: `cargo test -p redliner config:: cli::`
Expected: compile errors (no `replay` field or flags).

- [ ] **Step 3: Implement the structs, the loader, defaults, resolve, validation, and flags** per the rules above.

- [ ] **Step 4: Run the tests**
Run: `make test && make lint`
Expected: all pass. The existing e2e tests still pass, which proves profile mode is unchanged now that overrides run before validation.

- [ ] **Step 5: Commit** — `feat(config): replay block, scenario match rules, replay CLI flags`

---

### Task 3: Trace parser (JSONL and HAR)

**Files:**
- Create: `crates/redliner/src/replay/mod.rs` (module decls and `ReplayError`), `crates/redliner/src/replay/parse.rs`
- Create: `crates/redliner/tests/fixtures/replay/basic.jsonl`, `crates/redliner/tests/fixtures/replay/basic.har`
- Modify: `crates/redliner/src/lib.rs` (`pub mod replay;`)

**Interfaces:**
- Consumes: `ReplayConfig`, `ReplayFields`, `parse_rfc3339` (Task 2); `data::extract_path`.
- Produces:
  - `RawEvent { at: DateTime<Utc>, line: usize, method: String, path: String, operation: Option<String> }`. `method` is uppercased. `path` has no scheme/host/query/fragment and starts with `/`. For HAR, `line` is the 1-based entry index.
  - `Trace { events: Vec<RawEvent>, malformed: u64, malformed_examples: Vec<String>, filtered: u64 }`. `events` is stable-sorted by `at`. `malformed_examples` holds at most 3 entries, formatted `"line {n}: {reason}"`.
  - `pub fn read_trace(cfg: &ReplayConfig) -> Result<Trace, ReplayError>`: dispatches on `cfg.format` to `parse_jsonl(text: &str, fields: &ReplayFields) -> Trace` or `parse_har(text: &str) -> Result<Trace, ReplayError>`, then applies `window`.
  - `ReplayError` (thiserror) variants: `Io { path: PathBuf, source: std::io::Error }` (`"replay: read {path}: {source}"`) and `Har(String)` (`"replay: invalid HAR: {0}"`). Later tasks add variants.

**Rules:**
- JSONL: blank lines are skipped and not counted. Any other line must be a JSON object. `ts` is the first value at `fields.ts`: a string parses as RFC3339, a number is epoch seconds (fractional allowed). `method` and `path` are required strings. `operation` is optional; a missing, null, or non-string value gives `None`. Any failure makes the line malformed.
- Path normalization (both formats): strip `http(s)://host`, then cut at the first `?` or `#`. The result must start with `/`.
- HAR: `log.entries[]` → `startedDateTime`, `request.method`, `request.url`. `operation` is `operationName` from `request.postData.text` when that text parses as a JSON object. A document that is not JSON or has no `log.entries` array is `ReplayError::Har`. A bad entry is malformed.

- [ ] **Step 1: Write the failing tests** (inline `#[cfg(test)]` in `parse.rs`; fixture helper like `config/tests.rs::fixture`):
  - `jsonl_reads_events_in_time_order`: `basic.jsonl` holds 4 lines out of order, one stamped `+02:00`. Assert the ops come out in instant order and the offsets from the first event are `[0, 250ms, 1s, 1.5s]`.
  - `jsonl_epoch_seconds_timestamps`: `1760968800.5` and `1760968801` are 500ms apart.
  - `jsonl_skips_malformed_lines_and_counts_them`: input `"not json\n\n{\"method\":\"GET\",\"path\":\"/a\"}\n<valid>"` gives `malformed == 2`, `events.len() == 1`, and `malformed_examples[0].starts_with("line 1:")`.
  - `jsonl_custom_field_paths`: `fields {ts: time, method: req.method, path: req.url, operation: gql.op}` reads nested objects.
  - `path_is_normalized`: `"https://api.x/skus/1?x=y#f"` becomes `"/skus/1"`.
  - `window_filters_half_open`: events at `start`, mid, and `end` keep 2, with `filtered == 1`.
  - `har_reads_entries_and_graphql_operation`: `basic.har` has 2 entries. The POST entry's postData has `operationName: getCountry`, so `operation == Some("getCountry")`.
  - `har_invalid_document_errors`: `"{}"` gives `Err(ReplayError::Har(_))`.

- [ ] **Step 2: Run them to make sure they fail**
Run: `cargo test -p redliner replay::parse`
Expected: compile errors (module missing).

- [ ] **Step 3: Add chrono-based parsing, `parse_jsonl`, `parse_har`, `read_trace`, the fixtures, and the `ReplayError` variants.**

- [ ] **Step 4: Run the tests**
Run: `cargo test -p redliner replay::parse && make lint`
Expected: PASS.

- [ ] **Step 5: Commit** — `feat(replay): JSONL and HAR trace parser`

---

### Task 4: Matcher

**Files:**
- Create: `crates/redliner/src/replay/matcher.rs`

**Interfaces:**
- Consumes: `RawEvent`, `Trace` (Task 3); `Scenario.matcher`, `Protocol` (Task 2).
- Produces:
  - `Matched { at: DateTime<Utc>, scenario: usize }` (`scenario` indexes `cfg.scenarios`)
  - `MatchResult { matched: Vec<Matched>, per_scenario: BTreeMap<String, u64>, unmatched: u64, unmatched_top: Vec<(String, u64)> }`
  - `pub fn match_events(trace: &Trace, scenarios: &[Scenario], protocol: Protocol) -> Result<MatchResult, ReplayError>`
  - New variant `ReplayError::Ambiguous { line: usize, event: String, first: String, second: String }`, message `"replay: line {line} ({event}) matches both scenario {first:?} and {second:?}; make the match rules disjoint"`

**Rules:**
- GraphQL: `event.operation == Some(rule.operation)`, exact and case-sensitive.
- REST: when `rule.method` is set, it must equal `event.method` (ASCII case-insensitive). `rule.path` is a glob over `event.path`, compiled once with the `regex` crate: `**` becomes `.*`, `*` becomes `[^/]*`, every other character goes through `regex::escape`, and the whole pattern is anchored `^…$`.
- An event that matches two or more scenarios is a `ReplayError::Ambiguous` error.
- An event's key (used for `event` and the unmatched list) is `"{method} {path}"`, plus `" op={operation}"` when an operation is present. `unmatched_top` holds the 10 most frequent unmatched keys, sorted by count descending, then key ascending.
- Scenarios without `match` never match.

- [ ] **Step 1: Write the failing tests:** `graphql_matches_by_operation`; `rest_glob_single_segment` (`/skus/*` matches `/skus/1`, but not `/skus/1/reviews`); `rest_double_star` (`/skus/**` matches both); `rest_method_optional_and_case_insensitive`; `overlapping_rules_error_names_both` (the message contains both scenario names and `line 1`); `unmatched_tally_and_top_list_ordered`; `scenario_without_match_never_matches`.
- [ ] **Step 2: Run them to make sure they fail.** Run `cargo test -p redliner replay::matcher`. Expected: compile errors.
- [ ] **Step 3: Implement `match_events`** per the rules above.
- [ ] **Step 4: Run the tests.** Run `cargo test -p redliner replay::matcher && make lint`. Expected: PASS.
- [ ] **Step 5: Commit** — `feat(replay): scenario match rules`

---

### Task 5: Schedule builder with speed and copies

**Files:**
- Create: `crates/redliner/src/replay/schedule.rs`

**Interfaces:**
- Consumes: `Matched` (Task 4).
- Produces:
  - `ScheduledHit { at: Duration, scenario: usize }`
  - `Schedule { hits: Vec<ScheduledHit> }`. `at` is non-decreasing.
  - `pub fn build_schedule(matched: &[Matched], speed: f64, copies: u64) -> Schedule`
  - `impl Schedule { pub fn len(&self) -> usize; pub fn is_empty(&self) -> bool; pub fn span(&self) -> Duration /* last at, ZERO if empty */; pub fn peak_rps(&self) -> u64 /* max hits in any [t, t+1s) starting at a hit */ }`

**Algorithm** (copies rotate within the trace span, so every copy runs in parallel and total volume is `copies ×` at the same overall duration):

```text
o_i   = matched[i].at - matched[0].at        (as nanos)
S     = o_last
for k in 0..copies:
    shift = S * k / copies
    for each i: p = o_i + shift; if p > S { p -= S }; emit (p, scenario_i)
stable-sort all emitted hits by p      // ties: copy 0 first, then by original order
at = p / speed                         // f64 nanos → Duration
```

- [ ] **Step 1: Write the failing tests:**
  - `offsets_relative_to_first_event`: events at `.000`, `.250`, and `+1s` give `[0, 250ms, 1s]`.
  - `speed_divides_gaps`: speed `2.0` gives `[0, 125ms, 500ms]`; speed `0.5` gives `[0, 500ms, 2s]`.
  - `copies_multiply_hits_within_span`: 4 events over 3s with copies `3` give 12 hits, every `at <= 3s`, non-decreasing order, each scenario's count ×3, and copy 0's original offsets still present.
  - `copies_on_zero_span_trace`: 3 events at one instant with copies `2` give 6 hits, all at 0.
  - `peak_rps_counts_densest_second`, and `empty_schedule` (len 0, span 0, peak 0).
- [ ] **Step 2: Run them to make sure they fail.** Run `cargo test -p redliner replay::schedule`. Expected: compile errors.
- [ ] **Step 3: Implement** per the algorithm above.
- [ ] **Step 4: Run the tests.** Run `cargo test -p redliner replay::schedule && make lint`. Expected: PASS.
- [ ] **Step 5: Commit** — `feat(replay): schedule with speed and copies amplification`

---

### Task 6: Pacer, targeter, and `build_replay`

**Files:**
- Create: `crates/redliner/src/replay/targeter.rs`
- Modify: `crates/redliner/src/replay/mod.rs`

**Interfaces:**
- Consumes: `read_trace` (Task 3), `match_events` (Task 4), `build_schedule`/`Schedule` (Task 5), `Encoder`/`new_encoder`, `Pools`, the new `Targeter` signature (Task 1).
- Produces:
  - `SchedulePacer::new(schedule: Arc<Schedule>) -> Self`, which implements `volley::Pacer`:
    - `pace(elapsed, hits)` returns `(ZERO, true)` when `hits >= len`. Otherwise it returns `(at[hits].saturating_sub(elapsed), false)`, so a late hit fires immediately.
    - `rate()` is `len / span` in seconds, or `0.0` when the span is zero.
  - `ScheduleTargeter::new(enc: Box<dyn Encoder>, scenarios: Vec<Scenario>, pools: Pools, schedule: Arc<Schedule>, seed: u64) -> Self`, which implements `volley::Targeter`:
    - `target(seq, t)` encodes `scenarios[schedule.hits[seq].scenario]` with `let mut rng = ChaCha8Rng::seed_from_u64(seed); rng.set_stream(seq);`. It takes no lock.
    - An out-of-range `seq` returns `volley::Error::Build("replay: hit {seq} beyond schedule")`.
    - `pub fn operation(&self, seq: u64) -> Option<&str>` returns the scenario name, for report attribution.
  - `ReplaySummary` (`Serialize, Debug, Clone`): `{ source: String, parsed: u64, malformed: u64, malformed_examples: Vec<String>, filtered: u64, matched: BTreeMap<String, u64>, unmatched: u64, unmatched_top: Vec<(String, u64)>, hits: u64, span: String /* slo::fmt_dur */, speed: f64, copies: u64, peak_rps: u64 }`, plus `pub fn render(&self) -> String`, a multi-line text block that starts with `"replay: "` and lists each scenario count and the top unmatched keys.
  - `Replay { schedule: Arc<Schedule>, summary: ReplaySummary }`
  - `pub fn build_replay(cfg: &Config) -> Result<Replay, ReplayError>`: runs `read_trace`, then `match_events`, then the unmatched and empty checks below, then `build_schedule`, then builds the summary.
  - New variants:
    - `ReplayError::Unmatched { count: u64, top: String }`: `"replay: {count} log events matched no scenario (on_unmatched: fail); most frequent: {top}"`
    - `ReplayError::Empty { parsed: u64, malformed: u64, filtered: u64, unmatched: u64 }`: `"replay: no events left to replay (parsed {parsed}, malformed {malformed}, outside window {filtered}, unmatched {unmatched})"`
- Checks: when `on_unmatched == "fail"` and `unmatched > 0`, return `Unmatched`. When `matched` is empty (in either mode), return `Empty`.

- [ ] **Step 1: Write the failing tests:**
  - `pacer_waits_for_each_scheduled_hit`: schedule `[0, 100ms, 100ms, 1s]`. `pace(0,0)` gives `(0,false)`; `pace(50ms,1)` gives `(50ms,false)`; `pace(100ms,2)` gives `(0,false)`; `pace(2s,3)` gives `(0,false)`; `pace(2s,4)` gives `(_,true)`.
  - `targeter_is_deterministic_per_seq`: two targeters (`GraphqlEncoder`, variable `"{{ids}}"`, range pool `1..1_000_000`, same seed). One calls seq 5 then 2, the other 2 then 5; the bodies per seq are byte-identical.
  - `targeter_uses_scheduled_scenario`: schedule scenarios `[1, 0]`; the seq 0 body has `operationName` of `scenarios[1]`.
  - `build_replay_fail_mode_rejects_unmatched` and `build_replay_empty_is_error`: write a temp log; the messages contain the counts.
  - `summary_render_lists_scenarios_and_unmatched`.
- [ ] **Step 2: Run them to make sure they fail.** Run `cargo test -p redliner replay::`. Expected: compile errors.
- [ ] **Step 3: Implement `SchedulePacer`, `ScheduleTargeter`, `ReplaySummary::render`, and `build_replay`.**
- [ ] **Step 4: Run the tests.** Run `cargo test -p redliner replay:: && make lint`. Expected: PASS.
- [ ] **Step 5: Commit** — `feat(replay): schedule pacer, per-seq targeter, build_replay`

---

### Task 7: Run integration, report, and end-to-end tests

**Files:**
- Modify: `crates/redliner/src/run.rs`
- Modify: `crates/redliner/src/report.rs` (`RunManifest.replay`; add `replay: None` to the test literals at `:476` and `:502`)
- Test: `crates/redliner/tests/e2e.rs`

**Interfaces:**
- Consumes: `build_replay`, `Replay`, `SchedulePacer`, `ScheduleTargeter`, `ReplaySummary` (Task 6).
- Produces: `RunManifest.replay: Option<ReplaySummary>` with `#[serde(skip_serializing_if = "Option::is_none")]`.

**Requirements:**
- `--validate` with replay calls `build_replay`, prints `"{config}: valid ({proto}, {n} scenarios, replay {source})"` and then `summary.render()`, and returns `Exit::Ok`. It fires nothing and creates no report dir. A `ReplayError` is exit `2`.
- A replay run calls `build_replay` **before** the report dir is created. It then builds pools and runs harvest as usual, and runs **one** attacker: `attack(targeter, Arc::new(SchedulePacer::new(schedule.clone())), schedule.span() + Duration::from_secs(1), "replay".into())`. Each result goes through `checker.apply`, then `collector.add(&r, "replay", targeter.operation(r.seq))`. There is no warmup and no phases.
- Only the attack section branches. Report scaffolding, collector finalization, the SLO gate, JUnit, and the exit code stay on one shared path. Set `manifest.replay = Some(summary)`.
- The startup line in replay mode is `target={url} replay={source} hits={n} speed={speed} copies={copies} seed={seed}`.
- `print_summary` takes `replay: Option<&ReplaySummary>` and prints `render()` after the per-operation block.

- [ ] **Step 1: Write the failing e2e tests.** Add a helper that writes a GraphQL config with scenarios `a` and `b` (each with `match: {operation: …}`) and a JSONL log in a tempdir. The log has ops `a,b,a,b,a` spaced 150ms apart, plus one `health` event that matches nothing.
  - `e2e_replay_fires_each_matched_event_in_order`: `Exit::Ok`; `mock.hits == 5`; the received `operationName`s are `[a,b,a,b,a]`; the manifest has a `"replay"` key with `"unmatched": 1`.
  - `e2e_replay_paces_like_the_log`: the wall time of `run()` is at least 600ms and under 3s.
  - `e2e_replay_copies_and_speed`: `--copies 2 --speed 2` gives `mock.hits == 10`, a manifest with `"copies": 2` and `"speed": 2.0`, and a wall time under 1.5s. The exact gap math is pinned by the Task 5 unit tests, not by wall-clock timing.
  - `e2e_replay_strict_exits_error_without_firing`: `--strict` gives `Exit::Error` with `mock.hits == 0`.
  - `e2e_replay_validate_prints_schedule_without_firing`: `--validate` gives `Exit::Ok`, `mock.hits == 0`, and no `latest` symlink.
  - `e2e_replay_flag_rejects_profile_config`: `--replay log.jsonl` on the existing `write_cfg` constant-profile config gives `Exit::Error` with `mock.hits == 0`.
- [ ] **Step 2: Run them to make sure they fail.** Run `cargo test -p redliner --test e2e replay`. Expected: failures. Replay mode has no run path yet, so the profile path runs or validation errors.
- [ ] **Step 3: Implement** the `run.rs` and `report.rs` changes per the requirements above.
- [ ] **Step 4: Run the full suite.** Run `make test && make lint`. Expected: all pass, including the existing e2e tests.
- [ ] **Step 5: Commit** — `feat(run): replay mode end to end`

---

### Task 8: Documentation and example

**Files:**
- Modify: `docs/configuration.md` (a new `## replay` section after `## load`, and a `### match` subsection under `## scenarios`)
- Create: `configs/replay-example.yaml`, `configs/replay-sample.jsonl` (about 20 lines, two operations, one unmatched `GET /health`)
- Modify: `CHANGELOG.md` (an `Unreleased → Added` entry), `README.md` (a replay feature bullet and a usage example)

**Requirements:** The docs cover every `replay` key with its default, the JSONL field mapping and timestamp forms, the HAR fields read, the glob syntax (`*` and `**`), how `speed` and `copies` work (copies rotate within the span), what `--strict` does, the `load.*` fields that cannot be combined with replay, and that variable values come from pools, not the log.

- [ ] **Step 1: Write the docs, the example config, and the sample log.**
- [ ] **Step 2: Verify the example**
Run: `cargo run -q -p redliner -- --config configs/replay-example.yaml --validate`
Expected: exit 0; the output includes `replay` with scenario counts and an unmatched `GET /health` line.
- [ ] **Step 3: Commit** — `docs: production log replay`
