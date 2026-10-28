# Configuration reference

A Redliner run is driven by one YAML file (`--config`). CLI flags override
individual values but the file is the source of truth. Unknown keys are
rejected, so a typo fails the run before any traffic. With no file at all,
`redliner --url <url>` benchmarks one endpoint with every value below at its
default (see the README).

Durations use Go syntax: `250ms`, `2m`, `1h30m`, `0`.

The only hard requirements are `target.url` and at least one scenario (with
the field its protocol needs). Everything else has a default.

## Top level

```yaml
version: 1        # optional, informational
target:  { ... }
load:    { ... }
replay:  { ... }  # optional; switches the run to production-log replay
data:    { ... }  # optional
scenarios: [ ... ]
checks:  { ... }  # optional
slos:    { ... }  # optional
report:  { ... }  # optional
```

## `target`

| key          | type    | default    | notes |
| ------------ | ------- | ---------- | ----- |
| `url`        | string  | *required* | full endpoint for GraphQL; base URL for REST (scenario paths are appended) |
| `protocol`   | enum    | `graphql`  | `graphql` \| `rest` |
| `timeout`    | duration| `10s`      | per-request timeout |
| `headers`    | map     | `{}`       | sent on every request; credential values are redacted in `run_manifest.json` (see below) |
| `auth`       | object  | `none`     | see below; credentials are read from the environment, never the file |
| `keep_alive` | bool    | `true`     | reuse pooled connections between requests |
| `max_idle_conns` | int | `10000`    | max idle pooled connections per host |
| `http2`      | bool    | `false`    | negotiate HTTP/2 with prior knowledge (cleartext h2c or a known h2 server) |
| `insecure_tls` | bool  | `false`    | skip TLS certificate verification (`--insecure` / `-k`), for staging or in-cluster services with self-signed certs; covers the harvest probe too, and the run prints a warning when it's on |

`run_manifest.json` records the config with the value of any header whose name
contains `auth`, `cookie`, `key`, `token`, `secret`, `passw`, `session`,
`credential`, `signature`, `jwt`, `dpop` or `bearer` (case-insensitive)
replaced by `<redacted>`. This covers `target.headers`, `-H`, and scenario and
harvest headers. Values in URLs, bodies, variables and pool `values` are
written as-is: a header that takes a credential from a `{{pool}}` is redacted,
but the pool's inline `values` are not. Keep credentials in literal headers or
in `target.auth` (read from the environment).

### `target.auth`

| key             | type   | notes |
| --------------- | ------ | ----- |
| `type`          | enum   | `none` \| `bearer` \| `basic` \| `header` |
| `token_env`     | string | env var holding the token (`bearer`, `header`) |
| `header`        | string | header name to set (`header`) |
| `username_env`  | string | env var holding the username (`basic`) |
| `password_env`  | string | env var holding the password (`basic`) |

A required env var that is unset fails the run before any traffic, naming the
variable.

```yaml
auth:
  type: bearer
  token_env: BENCH_TOKEN      # Authorization: Bearer <value of $BENCH_TOKEN>
```

## `load`

| key        | type     | default   | notes |
| ---------- | -------- | --------- | ----- |
| `profile`  | enum     | `poisson` | `constant` \| `poisson` \| `ramp` \| `steps` |
| `rate`     | float    | `200`     | requests per second (`constant`, `poisson`); fractional rates such as `0.5` or `2.7` are honoured |
| `duration` | duration | `30s`     | measured attack length (`constant`, `poisson`, `ramp`) |
| `warmup`   | duration | `0`       | unmeasured warmup at 10% of the first phase's rate |
| `ramp`     | object   | —         | `{ from, to }` rates for `profile: ramp` |
| `steps`    | list     | —         | `[{ rate, duration }, ...]` for `profile: steps` |
| `workers`  | int      | `10`      | initial concurrency (informational) |
| `max_workers` | int   | `100000`  | hard cap on in-flight requests |
| `seed`     | int      | `0`       | `0` derives a seed from the wall clock and records it; a fixed value reproduces pacing and sampling exactly |

Rates must be finite. `constant` and each `steps` entry must also satisfy
`rate × duration > 1`: they fire their first hit at `1/rate`, and a hit due
exactly at the end is not sent, so a smaller load fires nothing and is
rejected (`poisson` and `ramp` fire their first hit at once). A load just over
the line can still miss its only hit by the timer's millisecond resolution;
the run then fails the `successful_requests` check.

```yaml
# ramp: climb linearly from 50 to 500 rps over `duration`
load: { profile: ramp, duration: 2m, ramp: { from: 50, to: 500 } }

# steps: hold each rate for its duration (find the throughput knee)
load:
  profile: steps
  steps:
    - { rate: 100, duration: 30s }
    - { rate: 250, duration: 30s }
```

## `replay`

Optional. Instead of a synthetic profile, replay a structured production log:
each log entry is matched to a scenario (see [`match`](#match)) and fired in
the log's order and at its inter-arrival pace, through the same attacker,
checks, SLOs, and reports as a profile run. The log decides *which operations
fire, in what order, and when*; request **values** (ids, SKUs, search terms)
still come from `data` pools (static or harvested), never from the log.

Replay is on when a `replay` block is present or `--replay <log>` is passed.
Without either, the run is a normal profile run and every `match` is ignored.

| key            | type     | default     | notes |
| -------------- | -------- | ----------- | ----- |
| `source`       | string   | *required*  | the log file; a relative path resolves against the config file's directory (a relative `--replay` path resolves against the current directory) |
| `format`       | enum     | inferred    | `json` \| `har`; inferred from the extension: `.har` is `har`, anything else is `json` (JSON lines) |
| `window`       | object   | —           | `{ start, end }` RFC3339 timestamps; replay only entries in the half-open range `[start, end)`; `start` must be before `end` |
| `speed`        | float    | `1.0`       | time compression: every gap is divided by `speed`, so `2.0` replays twice as fast. Must be > 0 |
| `copies`       | int      | `1`         | amplification: overlay `copies` copies of the log (see below) |
| `on_unmatched` | enum     | `skip`      | `skip` \| `fail`: what to do with entries no scenario matches |
| `fields`       | object   | see below   | JSONL field mapping |

```yaml
replay:
  source: logs/prod-2026-10-20.jsonl   # relative to this config file
  speed: 2.0                           # 2x faster than recorded
  copies: 3                            # 3x the recorded volume, same duration
  on_unmatched: fail                   # equivalent to --strict
  window:
    start: "2026-10-20T14:00:00Z"
    end:   "2026-10-20T15:00:00Z"
  fields:                              # only needed when your log differs
    ts: "@timestamp"
    method: http.method
    path: http.url
    operation: graphql.operationName
```

**Timeline.** Pacing starts at the first *matched* entry (time zero) and keeps
every later gap. Timestamps are never compared to the wall clock, so an old
log replays immediately.

**Amplification.** `speed` divides every offset. `copies` overlays N copies of
the matched log in parallel: copy *k* is rotated by *k*·span/N inside the
original span (offsets that run past the end wrap to the start). This
multiplies *total* and *average* volume by N over the *same* duration, but
because the copies are staggered rather than stacked, the busiest instants are
spread out: the **peak rate is not multiplied**. The `peak` value in the replay
summary reports the effective peak after this rotation. Use `speed` to compress
time and `copies` to multiply average load; they compose. The schedule is
capped at 10,000,000 total hits (after `copies`); a larger one is a config
error (exit `2`), so narrow the window or lower `copies`.

**`on_unmatched`.** Entries no scenario matches are counted and, by default,
skipped. `fail` (or `--strict`) aborts the run with exit `2`, before any
request, naming the most frequent unmatched entries.

### JSONL logs

One JSON object per line; blank lines are ignored. `replay.fields` gives the
dot path of each field in the object (same syntax as harvest `extract`:
`a.b.c`, with `[]` to fan out an array, using the first value found).

| `fields` key | default     | required | meaning |
| ------------ | ----------- | -------- | ------- |
| `ts`         | `ts`        | yes      | when the request happened. An RFC3339 string (`"2026-10-20T14:00:00.250Z"`, any UTC offset) or an epoch number whose **unit is inferred from magnitude**: `>= 1e17` is nanoseconds, `>= 1e14` microseconds, `>= 1e11` milliseconds, otherwise seconds (fractions allowed: `1760968800.25`) |
| `method`     | `method`    | yes      | HTTP method (case-insensitive) |
| `path`       | `path`      | yes      | request path or URL; the scheme, host, query string, and fragment are stripped, and the rest must start with `/` |
| `operation`  | `operation` | no       | GraphQL operation name; absent or non-string means "no operation" |

Extra fields are ignored. Replay does not use the log's bodies, headers, or
status codes. For a GraphQL log, `method` and `path` are still required on
every line even though matching uses only `operation`.

A line that is not valid JSON, is not an object, or lacks a required field
(or has an unusable timestamp or path) is **skipped and counted** as
malformed; the replay summary shows the count and the first three reasons.
Entries are stably sorted by time, so out-of-order logs are fine.

### HAR logs

`format: har` (or a `.har` file) reads a HAR 1.2 export: `log.entries[]`, each
using `startedDateTime` (RFC3339), `request.method`, and `request.url`. For
GraphQL, the operation is `operationName` in the JSON body at
`request.postData.text`, when present. `replay.fields` is ignored for HAR. A
document without `log.entries` fails the run; a bad individual entry is
counted as malformed like a bad JSONL line. In error and malformed messages,
"line N" for a HAR log is the 1-based index of the entry in `log.entries`, not
a physical file line.

### Validation and what combines with replay

Replay needs at least one scenario with a `match`. Every parse, match, and
schedule failure is exit `2` and happens **before any request is fired**.
The run also exits `2` when nothing is left to send (every entry malformed,
outside the window, or unmatched); the error lists the counts. The effective
span after `speed` must be at most 24h — a larger one (usually a wrong
timestamp unit or an outlier) is also exit `2`, so check the units, narrow
`replay.window`, or raise `--speed`.

Replay paces from the log, so `load.profile`, `rate`, `duration`, `warmup`,
`ramp`, and `steps` are **rejected** when replay is on (the error names the
field); the `--rate`, `--duration`, and `--warmup` flags map onto those fields
and are rejected the same way. `load.workers`, `load.max_workers`, and
`load.seed` still apply: the same config, log, and `seed` give the same
schedule and the same request values for every request, regardless of
concurrency.

The `--speed`, `--copies`, and `--strict` flags only tune an existing replay
run; passing any of them without `--replay <log>` or a `replay` block in the
config is a single error (exit `2`): `--speed, --copies and --strict need
--replay <log> or a replay block in the config`.

**Keeping up.** Replay fires at the log's pace through at most `load.max_workers`
concurrent requests. If the target (or the load generator) cannot keep up, the
run can reach its deadline with scheduled hits still unfired. Those dropped hits
are not silently ignored: the run prints a warning, records `fired`/`dropped` in
the replay summary and `run_manifest.json`, fails the `replay_delivery` check
(listed with the SLOs and in `junit.xml`), and **exits `1`** (even with no SLOs
enforced). Raise `load.max_workers` if the load generator is the bottleneck.
A run stopped with Ctrl-C still records `fired`/`dropped`, but skips the
`replay_delivery` check and exits `130` instead.

### Summary

`--validate` reads and matches the log without firing anything and prints this
block. A real run prints a one-line banner first (source, hits, span, speed,
copies, seed), then repeats the block in the final summary and stores it under
`replay` in `run_manifest.json`:

```
replay: configs/replay-sample.jsonl
  events        : 20 parsed, 0 malformed, 0 outside window
  matching      : 16 matched, 4 unmatched
  hits          : 16
  span          : 4.8s
  speed         : 1x
  copies        : 1
  peak          : 4 rps
  scenarios:
    GetProduct               10
    SearchProducts           6
  unmatched (top):
    GET /health (4)
```

`hits` is what will fire (after `copies`), `span` the replay duration (after
`speed`), and `peak` the most hits in any one-second window. Matched counts
are per scenario before `copies`. Results are attributed to scenarios by name
in the per-operation report, as in a profile run. After every real run (not
`--validate`) the block and manifest also carry a `fired` line
(`fired : 14 of 16 (2 dropped)`), and the SLO results gain a
`replay_delivery` check (also in `junit.xml`) that fails when any hit was
dropped — see **Keeping up** above.

## `data`

Optional. Named value pools feed `{{placeholder}}` templating; a harvest pass
can fill pools from the live service before the attack.

### `data.pools`

Each pool is keyed by name. `type` selects the source:

| `type`  | fields            | yields |
| ------- | ----------------- | ------ |
| `list`  | `values: [...]`   | a random element per draw; numbers keep their native type |
| `range` | `min`, `max`      | a random integer in `min..=max` |
| `file`  | `path`, `as`      | one line per entry; `as: int` (default `string`) parses to integers; blank lines and `#` comments are skipped |
| `uuid`  | —                 | a new random (v4) UUID string per draw, from `load.seed` |
| `sequence` | `start`        | the next integer per draw: `start`, `start+1`, ... (default `start: 1`) |
| `timestamp` | `as`          | the time of the draw: `rfc3339` (default, e.g. `2026-01-02T03:04:05.678Z`), `unix` (seconds) or `unix_ms`; `unix`/`unix_ms` are integers |

```yaml
data:
  pools:
    ids:          { type: range, min: 1, max: 1000 }
    country_codes:{ type: list,  values: [US, CA] }
    skus:         { type: file,  path: skus.txt, as: string }
    order_refs:   { type: uuid }
    order_no:     { type: sequence, start: 1000 }
    sent_at:      { type: timestamp, as: unix_ms }
```

File paths resolve relative to the config file.

Every placeholder is its own draw, so `{{order_no}}` twice in one body gives
two different numbers. The generator pools are unique *within* a run: the
sequence restarts at `start` and a fixed `load.seed` repeats the same UUIDs,
which is what makes a run reproducible. When values must also differ from the
previous run (idempotency keys against a persistent store), leave `seed` at
`0` (time-based) or pass a new `--seed`. `timestamp` reads the clock, so it is
the one pool a seed can't reproduce.

REST scenario `headers` take placeholders too, which is how an idempotency key
or correlation ID gets a fresh value on every request. They render after the
path and body, so adding one to a seeded config doesn't change the paths and
bodies it sends. `target.headers` and `-H` are sent as written: they're shared
by every request, including the harvest probe, which runs before the pools
exist.

### `data.harvest`

A list of probe requests run once, before the attack. Each extracts values by
JSON path and appends them into the named pools (creating a pool, or adding to
an existing one). A path segment ending in `[]` fans out across an array. If a
path matches nothing, the run fails with the pool and path named.

```yaml
data:
  harvest:
    - request: { query: "query{ country(code:\"US\"){ products{ id sku } } }" }
      extract:
        ids:  data.country.products[].id
        skus: data.country.products[].sku
    # REST form:
    # - request: { method: GET, path: /products?limit=100 }
    #   extract: { skus: items[].sku }
```

## `scenarios`

A weighted mix; one scenario is drawn per request, proportional to `weight`.
In [replay](#replay) mode `weight` is ignored and `match` picks the scenario
instead.

| key       | type   | notes |
| --------- | ------ | ----- |
| `name`    | string | used for per-operation report attribution |
| `weight`  | float  | relative selection weight; must be > 0 in profile mode, ignored in replay mode |
| `match`   | object | replay mode only: which log entries this scenario serves (see [`match`](#match)); ignored in profile mode |
| `request` | object | the per-protocol request (below) |

### `request` — GraphQL

| key         | type   | notes |
| ----------- | ------ | ----- |
| `query`     | string | inline GraphQL document (mutually exclusive with `query_file`) |
| `query_file`| string | path to a `.graphql` document, resolved relative to the config |
| `variables` | map    | `{{pool}}` placeholders are rendered per request |

### `request` — REST

| key       | type   | default | notes |
| --------- | ------ | ------- | ----- |
| `method`  | string | `GET`   | upper-cased |
| `path`    | string | —       | appended to `target.url`; may contain `{{pool}}` placeholders |
| `body`    | string | —       | request body; `{{pool}}` placeholders are rendered; sets `Content-Type: application/json` when present and none is given |
| `headers` | map    | `{}`    | per-scenario, override the base headers for that route only; values may contain `{{pool}}` placeholders (e.g. `Idempotency-Key: "{{order_refs}}"`), rendered per request after the path and body |

**Templating.** A value that is *exactly* one placeholder keeps the sampled
value's native type (`id: "{{ids}}"` encodes as a JSON number); an embedded
placeholder (`"/u/{{ids}}/x"`) interpolates as a string.

### `match`

Used only in replay mode; ignored otherwise, so one file can serve both
modes. A scenario with no `match` never receives replayed traffic. The fields
depend on `target.protocol`, and using the other protocol's fields is a
validation error.

| key         | protocol | notes |
| ----------- | -------- | ----- |
| `operation` | graphql  | required; the exact (case-sensitive) operation name |
| `method`    | rest     | optional; case-insensitive; omitted matches any method |
| `path`      | rest     | required; a glob that must start with `/` and match the whole logged path |

Path globs are matched against the logged path with the query string removed.
Everything is literal except `*`, which matches within one path segment (no
`/`), and `**`, which matches across segments at any depth.

| pattern            | matches                              | does not match |
| ------------------ | ------------------------------------ | -------------- |
| `/products/*`      | `/products/42`                       | `/products/42/reviews`, `/products` |
| `/products/*/reviews` | `/products/42/reviews`            | `/products/reviews` |
| `/orders/**`       | `/orders/9`, `/orders/9/items/3`     | `/order/9` |
| `/health`          | `/health`                            | `/health/live` |

```yaml
scenarios:
  - name: GetProduct
    match: { method: GET, path: /products/* }
    request: { method: GET, path: "/products/{{product_ids}}" }
  - name: CreateOrder
    match: { method: post, path: /orders }
    request: { method: POST, path: /orders, body: '{"productId": {{product_ids}}}' }
```

Each log entry must match **at most one** scenario. An entry that matches two
or more is an error (exit `2`, naming the log line and both scenarios), so
make rules disjoint. An entry matching none is unmatched (see
`replay.on_unmatched`). The scenario's `request` is what gets sent: the
matched log entry contributes only the scenario choice and its timing.

## `checks`

Optional. Reclassify responses that pass at the HTTP layer but fail at the
application layer. A reclassified result gets a zeroed status code and a
descriptive error, lowering the success rate and feeding the SLO gate. A
genuine transport error is never masked.

| key              | type       | default | notes |
| ---------------- | ---------- | ------- | ----- |
| `status`         | list[int]  | `[]`    | allowed HTTP statuses; empty means the default 2xx/3xx |
| `graphql_errors` | bool       | `true` for GraphQL, `false` for REST | a `200` carrying a non-empty top-level `errors` array counts as a failure |

## `slos`

Optional. Any threshold set is evaluated against the run's overall metrics; a
single violation makes the process exit `1`. A threshold left at `0` is not
enforced. Each one has a CLI override (`--slo-success-rate`, `--slo-p50`,
`--slo-p95`, `--slo-p99`, `--slo-p999`, `--slo-throughput-min`).

Independent of these thresholds, a run in which no request succeeded (none
fired, or every one failed at the transport layer or a check; a status listed
in `checks.status` counts as success) fails the
built-in `successful_requests` check, listed with the SLOs and in `junit.xml`,
and exits `1`. An interrupted run exits `130` instead.

| key             | type     | notes |
| --------------- | -------- | ----- |
| `success_rate`  | float    | minimum fraction of successful requests (e.g. `0.995`); must be 0–1 |
| `p50` / `p95` / `p99` / `p999` | duration | maximum latency at that percentile (`p999` is p99.9, the slowest 1 in 1000) |
| `throughput_min`| float    | minimum successful requests per second; must be >= 0 |

## `report`

Optional. Every run writes `report.dir/<UTC timestamp>/` and refreshes a
`latest` symlink; see the [README](../README.md#reports) for the file list.

| key      | type | default   | notes |
| -------- | ---- | --------- | ----- |
| `dir`    | string | `reports` | base directory for run folders (overridable with `--out`) |
| `junit`  | bool   | `false`   | also write `junit.xml` (SLO results as test cases) |
| `keep_raw` | bool | `true`    | also write `results.jsonl` (one JSON record per request) |
| `live_stats` | duration | `10s` | print a progress line on stderr this often (running total; rate, success, p95 and failures for the last window); `0s` turns it off, otherwise at least `100ms`. Warmup is not shown (overridable with `--live-stats`) |

### Accepted for compatibility (no effect)

These keys are accepted so configs written for the original Go harness still
load, but they are not applied by the current Rust client:

- `target.read_buffer_bytes`, `target.write_buffer_bytes` — reqwest does not
  expose socket read/write buffer sizing.
- `report.per_operation` — per-operation reporting is always emitted when any
  operation is attributable.
