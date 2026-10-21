# Production Log Replay — Design

**Date:** 2026-10-21
**Status:** Approved direction; spec under review
**Topic:** Ingest a structured production request log and run a load test that
reproduces the **order and pace** of the observed traffic, with optional
amplification, reusing Red Liner's existing scenarios, checks, SLOs, and
reports.

## Intent

Today a Red Liner "test" is one YAML scenario firing a **stochastic weighted
mix** of GraphQL/REST operations, paced by a synthetic profile
(`constant` / `poisson` / `ramp` / `steps`), concurrently. There is no notion
of request *order* and no log ingestion anywhere in the tree.

The goal is to let a maintainer take a **structured production log** (one
request window from the real service) and replay it as a test that fires the
**same operation mix, in the same order, at the same real inter-arrival pace**
— so the test reflects what production actually did rather than a synthetic
guess. The replay must also be able to **amplify** that traffic (go faster,
and/or overlay copies) for stress and headroom testing. Success means: given a
log file and a scenario catalog, `redliner` builds a time-accurate schedule,
fires it through the existing attacker, and gates on the existing SLOs — with a
dry-run (`--validate`) that lets the operator inspect the schedule before
firing.

### Decisions captured from brainstorming

- **Log format:** **rich structured** — JSON lines and HAR. Each entry carries
  a timestamp plus enough request detail (method, path, GraphQL
  `operationName`) to classify it.
- **Pace:** **both, configurable** — reproduce the exact observed inter-arrival
  timing by default; allow time-scaling / amplification on top.
- **Reconstruction:** **map onto existing scenarios.** The log drives
  order + pace + mix only; the actual request bytes are regenerated through the
  existing encoder + data pools. This keeps checks/SLOs/pools working and
  avoids replaying stale or sensitive production payloads.
- **Matching:** **explicit match rules in config.** Each scenario declares what
  log entries it matches. Unmatched entries are reported and **skipped** by
  default; a strict mode fails instead.
- **Amplification:** **two independent knobs** — a `speed` factor (time
  compression) and a `copies` factor (parallel overlays of the trace).

## Non-goals (YAGNI)

Explicitly out of scope, left as documented extension points:

- **Harvesting real variable values out of the log into pools.** Variable
  values still come from configured data pools, not from the recorded payloads.
  (The log shapes *which* operations fire and *when*, not *with what data*.)
- **Log-format auto-sniffing** beyond file-extension detection.
- **Streaming / out-of-core logs.** v1 loads the parsed trace into memory.
- **Verbatim payload replay.** Deliberately rejected per the reconstruction
  decision above.
- **Loop-to-fill-duration amplification.** Only `speed` and `copies` ship in
  v1; looping can be added later as a third transform if wanted.
- **New protocols.** Replay works with the existing GraphQL and REST encoders.

## Architecture

The replay feature **pre-materializes a schedule** once — after
parse → match → amplify — then feeds it to the **existing `volley::Attacker`**
through a thin pacer/targeter pair. This reuses the attacker's concurrency
(semaphore-bounded `max_workers`), shared `reqwest::Client`, metrics collector,
app-layer checks, SLO gate, and report directory wholesale.

```
replay.source (JSONL | HAR)
        │  parse  (crates/redliner/src/replay/parse.rs)
        ▼
  Vec<RawEvent { ts, method, path, operation, .. }>   (ordered by ts)
        │  match  (crates/redliner/src/replay/matcher.rs)
        ▼                                   ┌─ unmatched → report + skip|fail
  Vec<(RawEvent, &Scenario)>  ──────────────┘
        │  build + amplify  (crates/redliner/src/replay/schedule.rs)
        │    • at = ts - first_ts
        │    • speed:  at /= speed
        │    • copies: replicate N×, stagger by trace span, merge-sort by `at`
        ▼
  Schedule = Vec<ScheduledHit { at: Duration, target: Target }>
        │
        ▼
  SchedulePacer  (gap to next hit's `at`)  +  ScheduleTargeter (target by index)
        │
        ▼
  volley::Attacker  →  Metrics / Collector  →  checks  →  SLO gate  →  report dir
```

### Components

**`crates/redliner/src/replay/parse.rs`** — a `TraceReader` trait with `json`
(JSON-lines) and `har` implementations. Produces an ordered `Vec<RawEvent>`.
`RawEvent` holds the timestamp and the fields needed for matching (method,
path/URL, GraphQL operation name). Format is chosen by the `replay.format`
field, defaulting to file-extension detection (`.jsonl`/`.json` → json,
`.har` → har). An optional `replay.window { start, end }` filters events by
timestamp at parse time.

**`crates/redliner/src/replay/matcher.rs`** — compiles each scenario's new
`match` block into a matcher and classifies every `RawEvent` to a scenario id
or "unmatched". GraphQL scenarios match on `operation` (operation name); REST
scenarios match on `method` + a `path` glob. The matcher can reuse the existing
`Encoder::operation(method, url)` seam (`encode/mod.rs:45`) where helpful.
Unmatched events are tallied; `on_unmatched: skip` (default) drops them,
`on_unmatched: fail` returns a config/runtime error (exit `2`).

**`crates/redliner/src/replay/schedule.rs`** — turns matched events into
`ScheduledHit`s: `at` is each event's offset from the first retained event.
Amplification is then two pure list transforms:
- `speed` (default `1.0`): divide every `at` by the factor (`2.0` ⇒ same hits,
  half the gaps, double the instantaneous rate).
- `copies` (default `1`): replicate the whole schedule `N` times, offsetting
  copy *k* by `k × trace_span` (or a configurable stagger), then merge-sort all
  hits by `at`. Preserves per-copy order and overall shape at `N×` volume.

The `target: Target` in each `ScheduledHit` is pre-encoded at build time via
the matched scenario + the existing encoder + pools, so firing is allocation-
light and fully reproducible from `load.seed`.

**`SchedulePacer` + `ScheduleTargeter`** (in `redliner`, implementing the
`volley` traits) — `SchedulePacer::pace(elapsed, hits)` returns the wait until
`schedule[hits].at` and stops once `hits == schedule.len()`;
`ScheduleTargeter` returns `schedule[idx].target`. Both index the same
materialized `Schedule`, so order and pace stay in lockstep by construction.

### Required engine change

`volley::Targeter::target` currently takes only `&mut Target`
(`crates/volley/src/pacer.rs:19`) — no hit index — so a targeter cannot know
*which* scheduled hit it is filling. The trait gains the hit index:

```rust
fn target(&self, hits: u64, t: &mut Target) -> Result<(), Error>;
```

`MixTargeter` ignores the new argument (its pick stays random/seeded), so the
change is behavior-preserving for the existing path; `ScheduleTargeter` uses it
to look up `schedule[hits]`. The attacker already tracks the hit count it feeds
to `Pacer::pace`, so it passes the same value to the targeter.

### Config surface

A new top-level `replay` block, **mutually exclusive** with `load.profile`
(validation rejects having both, mirroring the existing `deny_unknown_fields` /
protocol-aware validation in `config/mod.rs`):

```yaml
replay:
  source: prod-2026-10-20.jsonl   # path, relative to config dir like other files
  format: json | har              # optional; inferred from extension
  window:                         # optional timestamp filter
    start: 2026-10-20T14:00:00Z
    end:   2026-10-20T14:05:00Z
  speed: 2.0                      # time-compression factor (default 1.0)
  copies: 3                       # parallel trace overlays (default 1)
  on_unmatched: skip | fail       # default skip
```

Each `scenario` gains an optional `match` block:

```yaml
scenarios:
  - name: getCountry
    request: { ... }              # unchanged; used to encode the request
    match:
      operation: getCountry       # GraphQL: match by operation name
  - name: listSkus
    request: { method: GET, path: /skus }
    match:
      method: GET
      path: /skus/*               # REST: method + path glob
```

In replay mode, scenario **`weight` is ignored** — scenarios act as a match
catalog, not a probability distribution. Validation warns if weights are set
alongside `replay`.

CLI overrides (clap, `cli.rs`): `--replay <file>` (sets `replay.source` and
switches to replay mode), `--speed`, `--copies`, `--strict` (⇒
`on_unmatched: fail`). These follow the existing "apply only flags the user
set" override pattern (`cli.rs:41`).

### Run integration

`run_inner` (`crates/redliner/src/run.rs:67`) branches on whether `replay` is
present:
- **Replay mode:** parse → match → build+amplify schedule → construct a
  `SchedulePacer` + `ScheduleTargeter` → run a **single** `Attacker` over the
  whole schedule (no phases, no synthetic warmup). Everything downstream
  (collector, checks, SLO gate, report dir, exit code) is unchanged.
- **Profile mode:** the existing phase/mix path, untouched.

### Fidelity

Order and real inter-arrival pace come directly from the log timestamps.
Concurrency/overlap is **emergent**, exactly as in the profile path and in
production: the attacker fires each scheduled hit at its `at` without awaiting
the response, bounded by `max_workers`, so slow responses naturally overlap
later hits. Request *values* come from pools (by decision), so the replay
reproduces the production **traffic shape**, not its exact payloads.

## Testing

- **Parser** (`replay/parse.rs`): fixture-based unit tests for JSONL and HAR —
  ordering by timestamp, field extraction (method/path/operation), window
  filtering, and malformed-entry handling.
- **Matcher** (`replay/matcher.rs`): GraphQL operation match, REST method+path
  glob match, unmatched tally, `skip` vs `fail` behavior.
- **Schedule** (`replay/schedule.rs`): `at` offsets from first event; `speed`
  halves/doubles gaps correctly; `copies` yields `N×` hits with correct stagger
  and a globally sorted `at` order; empty-schedule edge case.
- **Engine change** (`volley`): `Targeter` trait signature change compiles and
  `MixTargeter` behavior is unchanged (existing attacker/targeter tests still
  pass with the index threaded through).
- **End-to-end** (`crates/redliner/tests/e2e.rs`): a new replay test driving
  `run(Args)` against the existing mock TCP HTTP server with a small fixture
  log, asserting the fired request count, per-scenario attribution in the
  report, and the match summary (parsed / matched / skipped / unmatched).
- **Validation**: `--validate` in replay mode prints a schedule summary (total
  hits, span, peak instantaneous rate, per-scenario counts) and exits `0`
  **without firing**, so an operator can sanity-check an amplification first.
  Covered by a smoke/e2e assertion.

## Reporting

The run report gains a **replay match summary**: events parsed, matched per
scenario, skipped (unmatched), and the effective schedule stats (hit count,
span, speed, copies). This surfaces in the human-readable text summary and the
structured `MetricsJson` (`report.rs`), so a CI reader can see how faithfully
the log was reproduced. Existing per-request `results.jsonl`, checks, SLO gate,
and JUnit output are unchanged.

## Open risks / notes

- **HAR size:** HAR files can be large; v1's in-memory load is a deliberate
  simplicity trade (see non-goals). The parser is a trait, so a streaming
  reader can be added later without touching the schedule or run paths.
- **Clock skew / coarse timestamps:** if log timestamps are second-granular,
  many hits collapse to the same `at`; the attacker fires them back-to-back,
  which is acceptable (it matches the information the log actually carries).
- **`match` ambiguity:** if two scenarios match the same event, validation
  rejects the config at load time (first-match-wins is avoided to keep replays
  unambiguous).
