# Changelog

All notable changes to Redliner are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed
- `scenarios[].weight` defaults to `1`, so a one-scenario config (the README's
  "minimal config") no longer needs it. An explicit `0` or negative weight is
  still an error.

### Removed
- Release archives for Windows, Intel macOS, ARM Linux and glibc Linux.
  Releases ship two targets: static x86_64 Linux (musl) and Apple Silicon
  macOS. `install.sh` now stops with a build-from-source hint on ARM Linux
  and Intel macOS; elsewhere, build from source (`cargo install --git
  https://github.com/danny-oneal/redliner redliner redliner-import --locked`).

### Added
- `volley::Metrics::add_with_success`, for callers whose idea of success is
  not "no error and a 2xx/3xx code". `Metrics::add` keeps that default.

### Fixed
- A status listed in `checks.status` (a 404 you expect, say) now counts as a
  success everywhere: the success rate, throughput, per-operation and
  per-phase tables, live lines, `metrics.json`, and the `success_rate` and
  `throughput_min` SLOs. Only the exit code and the built-in
  `successful_requests` check honoured it before, so such a run understated
  its success rate and throughput (0% when every response was an allowed
  status) and could fail its SLOs. A report written before this fix and one
  written after it are not comparable for such a config.
- A `user:password@` in a config file's `target.url` is a validation error, as
  it already was for `--url`. It used to be written to `run_manifest.json`,
  every `results.jsonl` line and the stderr banner. Use `target.auth` or a
  header instead. A `target.url` that isn't an `http://` or `https://` URL is
  rejected too, instead of failing every request.
- `--validate` catches configs that would fail every request or stop the run
  before traffic, and so does every run, before it creates a report dir: a
  REST `path` without a leading `/`, an invalid HTTP method, an invalid header
  name or value, a `{{placeholder}}` that names no pool or harvest `extract`
  key, and (`--validate` now builds the pools) a missing or unreadable file
  pool. An unset `token_env`-style variable, which fails a run, is a warning
  under `--validate`, so configs can be checked without the secret.
- Harvest can extract from a response that is a top-level JSON array
  (`extract: { skus: "[].sku" }`), as the docs said `[]` would. A path that
  doesn't fit the response now names its pool in the error.
- Pool validation: a file pool's `as` must be `string` or `int` (`as: Int`
  used to mean string), a `range` pool needs `min`/`max` (it used to yield
  only `0`), and a field the pool's type doesn't use (`as` on a `list`, `min`
  on a `sequence`) is an error instead of being ignored. A negative
  `target.max_idle_conns` is rejected; it used to turn connection pooling off.
- A `weight` of `.nan` or `.inf`, or weights whose total overflows, is
  rejected instead of leaving a scenario that never fires.
- A `checks.status` entry outside 100–599 is rejected; it used to wrap
  around (`65736` meant `200`).
- `--rate` (or `load.rate`) with a `ramp` or `steps` profile, and
  `--duration` (or `load.duration`) with `steps`, are errors instead of being
  silently ignored.
- Two runs started in the same second into the same report directory
  (parallel CI jobs, say) no longer share one run folder and overwrite each
  other's files: the later one gets `<timestamp>-1`, `-2`, …, and `latest`
  points at the folder each run actually used.
- Every `results.jsonl` record now carries its `phase` and its send time
  (`timestamp_ns`, Unix nanoseconds), so the stream can be plotted over time;
  `seq` restarts every phase, so it alone could not order or place a hit.
- `run_manifest.json` writes config durations as text the config accepts
  back (`"timeout": "10s"`, `"5400s"`, `"1.5ms"`) instead of raw
  nanoseconds (`"10000000000ns"`).
- Error messages print each cause once: a YAML parse error no longer repeats
  the parser's message, and a missing pool file reads `pool "ids": read
  ids.txt: No such file or directory` instead of naming no path and repeating
  the OS error three times.
- A run whose stdout is closed before the summary is printed
  (`redliner ... | head -1`) no longer panics with exit code 101; it exits
  with its verdict (0, 1 or 130).
- An interrupted run's summary labels its first line `run           :
  INTERRUPTED (partial results)`; it shared the `status        :` label with
  the status-class line, so a script matching that label read the wrong one.
- A report file that can't be written no longer hides the run's result: the
  summary (with the SLO verdict) is printed first, then the error, which now
  names the file (`write reports: reports/…/metrics.json: Is a directory`
  instead of `Is a directory`). The exit code is still 2.
- Docs: the README's minimal config and two `docs/configuration.md` examples
  (the `data.pools` block and the REST harvest form) now parse. A harvest
  path containing `[]` must be quoted inside `{ ... }`.
- `scripts/install.sh` accepts `REDLINER_VERSION` without the leading `v`
  (`0.5.0` as well as `v0.5.0`) instead of failing with a 404.
- Replay: when no events are left to replay, the error now shows the first
  three malformed-line reasons (`line 1: missing field "ts"`), not only the
  counts, so a wrong `replay.fields` mapping says what is wrong.
- Replay: the summary lists every scenario with a `match` rule, so one that
  matched nothing (a typo'd `match.operation`) shows `0` instead of being
  left out.
- Replay: a JSONL epoch timestamp logged as a quoted string
  (`"ts":"1760968800"`, as nginx `$msec` writes it) is read as the number
  instead of making the line malformed.
- Replay: `speed: 0`, `copies: 0`, `--speed 0` and `--copies 0` are
  validation errors (exit `2`) instead of silently meaning `1`.
- Replay: two scenarios with an identical `match` rule are a validation
  error, even when the log has no entry they would both match.
- Replay: a UTF-8 byte-order mark at the start of a JSONL log no longer makes
  the first line malformed (dropping its event).
- Docs: `replay.copies` multiplies the peak rate by up to N, not only the
  average; `docs/configuration.md` said the peak was not multiplied.
- Replay reads a JSONL log line by line instead of loading the whole file
  first, so peak memory no longer includes a copy of the raw log (a 132 MB
  log: about 300 MB to 220 MB).
- GraphQL scenario `headers` and every harvest request's `headers` are now
  sent. They were silently dropped, though `run_manifest.json` listed them.
  GraphQL scenario headers take `{{pool}}` placeholders, as REST ones do.
- A GraphQL scenario's `name` is sent as `operationName` only when its query
  defines an operation by that name. It was always sent, so spec-compliant
  servers rejected every anonymous query (the README's own example) with
  `Unknown operation named ...`.
- A fixed `load.seed` now reproduces which scenario and pool values each hit
  sends in a profile run. All workers shared one random stream, so the draw
  order followed task scheduling; each hit now draws from its own stream, as
  replay hits already did. A seeded run sends different values than the same
  seed did before.
- Per-operation reports count every REST hit. Hits were matched back to their
  scenario from the URL, which lost any hit whose sampled value held `/`, `&`
  or `?`; they are now attributed by the scenario that built them.
- REST pool values are percent-encoded where a `path` splices them in, so
  `sku={{skus}}` with the value `a&b=c` sends one `sku` parameter, not two,
  and a value holding `/` or `?` no longer changes the route. Only the
  template text of `path` is sent as written; bodies and headers are
  unchanged.
- A GraphQL `target.url` with a query string (`/graphql?tenant=t1`) keeps
  it intact: the `op` tag is joined with `&`. It was appended after a second
  `?`, so the server read `tenant` as `t1?op=<scenario>`.
- A GraphQL variable like `"{{ids}}}"` interpolates as the string `5}`
  instead of failing every request with `unknown pool ids}`, and an empty
  `{{}}` is reported as such instead of as `unknown pool` with no name.
- Harvest probes accept any 2xx response (a `201`, say), not only `200`, and
  connect as the attack does: over HTTP/2 when `target.http2` is set, and
  without reusing connections when `target.keep_alive` is `false`.

## [0.5.1] - 2026-10-31

### Changed
- Release archives, `scripts/install.sh` and the Docker image now include
  `redliner-import` next to `redliner`. The install script still installs
  releases from before it shipped (`REDLINER_VERSION=v0.5.0`).

### Fixed
- `redliner-import`:
  - A path parameter the spec doesn't declare (or declares in another case)
    gets a TODO pool instead of being sent as a literal `{name}`.
  - A base URL that isn't `http://` or `https://` (a server written
    `localhost:8080/api`, or `--base-url api.example.com`) is an error instead
    of a draft whose every request fails.
  - Numeric server-variable defaults (`default: 8080`) are substituted.
  - A spec whose path items are all external `$ref`s says how many were
    skipped and suggests bundling it, instead of only "no usable operations".
  - `Accept` and `Content-Type` header parameters are ignored, as OpenAPI
    requires, instead of becoming scenario headers.
  - A GraphQL endpoint that redirects is reported with the URL it redirects
    to, instead of as "introspection is disabled".
  - Request body examples under `schema.examples` (FastAPI, pydantic v2) are
    used.

## [0.5.0] - 2026-10-31

### Added
- `redliner-import`, a separate tool that scaffolds a Redliner config from a
  live GraphQL endpoint (introspection) or an OpenAPI 3.x / Swagger 2.0
  document. State-changing operations are commented out unless
  `--include-unsafe`. Credentials used to fetch the schema (`-H` values, the
  `--bearer-env` token, a `user:password@` in a URL, a spec URL's query) are
  never written to the draft, and text from the spec or server can't break out
  of the draft's comments. Install from source: `cargo install --path crates/redliner-import`.

## [0.4.1] - 2026-10-27

### Security
- `run_manifest.json` no longer stores credential header values
  (`Authorization`, `Cookie`, `*key*`, `*token*`, `*secret*`, …) from
  `target.headers`, `-H`, or scenario and harvest headers; they are written as
  `<redacted>`.

### Changed
- A run in which no request succeeded now exits `1` (built-in
  `successful_requests` check) even with no SLOs configured; previously a
  down target passed.

### Fixed
- `constant`, `steps` and `ramp` no longer truncate rates to whole numbers:
  `rate: 0.5` fired nothing and `rate: 2.7` ran at 2 rps.
- Validation rejects non-finite rates (`.nan`, `.inf`), and `constant` or
  `steps` loads too small to fire a request (`rate × duration ≤ 1`).

## [0.4.0] - 2026-10-27

### Added
- **Placeholders in REST scenario headers:** `headers` values in a REST
  scenario take `{{pool}}` placeholders, rendered per request. For example,
  `Idempotency-Key: "{{order_refs}}"` with a `uuid` pool sends a fresh key on
  every request. They render after the path and body, so adding one to a
  seeded config doesn't change the paths and bodies it sends. `target.headers`
  and `-H` are still sent as written.
- **p99.9 latency** in the run summary, `text.txt` and `metrics.json`
  (`latencies.p999`), with an SLO for it: `slos.p999` / `--slo-p999`.
- **Status classes** in the run summary and `text.txt`: one line such as
  `status : 2xx 480, 4xx 12, 5xx 3, no response 5`, so 4xx/5xx responses and
  requests that never got a response stand out without reading the full
  status-code list.
- **`target.insecure_tls` / `--insecure` (`-k`)** skips TLS certificate
  verification, so runs can reach staging or in-cluster services with
  self-signed certs. It covers the harvest probe as well as attack traffic,
  and every run (and `--validate`) that has it on prints a warning naming
  the target. Off by default.

### Changed
- `volley::Latencies` has a new `p999` field. This breaks external code that
  builds the struct without `..Default::default()`.
- `volley::AttackerConfig` has a new `danger_accept_invalid_certs` field
  (default `false`). This breaks external code that builds the struct
  without `..Default::default()`.

### Fixed
- A latency percentile can no longer read higher than `max`. The histogram
  reports a bucket's upper edge, so p99 could show e.g. `784µs` against a
  `max` of `783µs`; percentiles are now capped at the slowest request.

## [0.3.0] - 2026-10-27

### Added
- **`redliner compare <baseline> <candidate>`** prints the change between two
  runs' reports: p50/p95/p99, throughput and success rate, overall, per load
  phase (for `steps` runs) and per operation. With `--max-regression 10%`,
  it exits `1` when a latency percentile rises, or throughput falls, by more
  than 10% of the baseline, so CI can fail a build that got slower than the
  last one. `--junit <file>` also writes each gated metric as a JUnit test
  case, so CI test reports name the ones that regressed. It exits `2` when a
  report can't be read or the JUnit file can't be written, and warns when
  the runs fired at different rates.

## [0.2.0] - 2026-10-27

### Added
- **Production-log replay** — replay a structured production log (JSON lines
  or HAR) instead of a synthetic profile. Entries are matched to scenarios by
  per-scenario `match` rules (GraphQL `operation`; REST `method` + a `*` / `**`
  path glob) and fired in the log's order and at its inter-arrival pace through
  the existing attacker, checks, SLOs, and reports. Switch on with a `replay`
  block or `--replay <log>`; `replay.speed` / `--speed` compress time,
  `replay.copies` / `--copies` multiply volume over the same duration,
  `replay.window` slices the log, and `replay.on_unmatched: fail` / `--strict`
  aborts on unmatched entries. Request values still come from data pools.
  Malformed log lines are skipped and counted, a replay summary is printed and
  stored in `run_manifest.json`, and every parse/match failure is exit `2`
  before any request is fired. Epoch timestamp units (s/ms/µs/ns) are inferred
  by magnitude; the post-`speed` span is capped at 24h and the schedule at
  10,000,000 hits (both exit `2`); and a replay that falls behind records
  `fired`/`dropped`, fails a `replay_delivery` check (also in `junit.xml`),
  warns, and exits `1`. See `configs/replay-example.yaml`.
- **Live stats** — a progress line on stderr every 10s during the attack: the
  running request total plus the rate, success, p95 and failures of the last
  window. Set the interval with `report.live_stats` or `--live-stats` (`0s`
  turns it off; anything else must be at least `100ms`).
- **Zero-config runs** — `redliner --url https://host/path?q=1` benchmarks one
  endpoint with no config file: the path and query become a single REST
  scenario and everything else takes its default. `--method`, `--body` and
  `-H`/`--header` describe the request, and the usual flags (`--rate`,
  `--duration`, ...) still apply. A `redliner.yaml` in the current directory
  still takes precedence, `--header` also adds to a config file's
  `target.headers`, and running with neither a config nor `--url` now says so
  in one line instead of failing to read `redliner.yaml`.
- **Generator pools** — `uuid` (seeded v4 UUIDs), `sequence` (counts up from
  `start`, default 1) and `timestamp` (`rfc3339`, `unix` or `unix_ms`) pool
  types produce unique values such as idempotency keys, emails and
  correlation IDs with no data file. UUIDs come from `load.seed`, so a seeded
  run repeats them exactly.
- **SLO flags** — `--slo-success-rate`, `--slo-p50`, `--slo-p95`, `--slo-p99`
  and `--slo-throughput-min` override the `slos` block, so a zero-config run
  can gate CI in one line (`redliner --url ... --slo-p95 200ms`). `0`/`0s`
  turns a file's SLO off.
- **Graceful Ctrl-C** — the first Ctrl-C stops firing, lets in-flight requests
  finish, and writes the full report directory from the partial results
  (`"aborted": true` in `run_manifest.json`, an `INTERRUPTED` line in the
  summary), then exits `130`. A second Ctrl-C quits immediately without
  reports. Library users get `redliner::run_with_interrupt` and
  `volley::Attacker::attack_until`, which stops on a `watch` flag even mid-wait.

- **Static Linux binaries and an install script** — releases now include
  static musl builds for x86_64 and arm64 Linux, and every release asset has a
  `.sha256` checksum. `scripts/install.sh` picks the right asset, verifies the
  checksum (and refuses a release without one), and installs to
  `~/.local/bin`.

### Changed
- `slos.success_rate` must be a fraction from 0 to 1 and `slos.throughput_min`
  must be >= 0 (exit `2`). A percentage such as `99.5` used to be accepted and
  then failed every run.
- The engine crate is now the `redliner-volley` package (`volley` on
  crates.io is an unrelated crate), so both crates can be published. Code
  still imports it as `volley`; a git or path dependency on it needs
  `volley = { package = "redliner-volley", ... }`.
- Release builds use fat LTO, one codegen unit and stripped symbols, and the
  binary uses mimalloc. At 5000 rps against a local server the load
  generator's own CPU per request is ~28% lower, and the Linux binary shrinks
  from 10.9 MB to ~6 MB.
- `volley::Targeter::target` now takes the hit index `seq: u64` (breaking for
  external `Targeter` implementations).
- CLI overrides are now applied before defaults and validation, so overridden
  values are validated, `--seed 0` derives a wall-clock seed, and `--rate 0` /
  `--duration 0s` fall back to defaults.

### Fixed
- `redliner --version` reports the release's version. Releases 0.1.1–0.1.5
  were tagged without bumping the crates and all reported `0.1.0`. Releases
  are now cut from the crate version: CI tags `v<version>` when a version bump
  merges, refuses a tag that doesn't match it, and fails a binary that
  reports anything else.
- Templating no longer re-parses sampled values: a pool value containing `{{`
  (e.g. harvested or file-pool data) is inserted verbatim instead of failing
  the request as an unknown pool or bad template, and a value that references
  its own pool no longer hangs the run.

## [0.1.0] - 2026-10-20

Initial release — a from-scratch Rust port of an internal Go/Vegeta harness.

### Added
- **`volley` engine** — protocol-agnostic async load generation: `Target` /
  `AttackResult` / `Metrics`, the `Pacer` and `Targeter` traits, constant /
  Poisson (seeded, self-correcting) / linear pacers, and a tokio `Attacker`
  that fires at a paced rate over reqwest.
- **Declarative YAML config** with defaults, fail-fast validation (unknown keys
  rejected), Go-style durations, and config-relative file resolution.
- **GraphQL and REST** encoders behind one seam, selected by `target.protocol`,
  with env-sourced auth (`none` / `bearer` / `basic` / `header`) and a weighted
  operation mix.
- **Load profiles** `constant`, `poisson`, `ramp`, and `steps`, with an
  unmeasured warmup phase.
- **Data pools and templating** — `{{pool}}` placeholders in paths, bodies, and
  GraphQL variables, drawn from `list` / `range` / `file` pools, preserving
  native scalar types.
- **Harvest** — a pre-attack probe that seeds pools from the live service by
  JSON-path extraction, so every request resolves to real data.
- **App-layer checks** — reclassify a `200`-with-GraphQL-`errors` or an
  out-of-set status as a failure so the success rate and SLO gate tell the
  truth; transport errors are never masked.
- **SLO gate** on success rate, p50/p95/p99, and throughput, driving the exit
  code (`0` pass, `1` SLO violated, `2` config/runtime error).
- **Per-run report directory** — `reports/<timestamp>/` (plus a `latest`
  symlink) with a text summary, latency histogram, `metrics.json`, per-operation
  and per-phase breakdowns, `run_manifest.json`, `junit.xml`, and a raw
  `results.jsonl` stream.
- **Redline banner** — a tachometer bar printed in the run summary when a run
  violates one or more SLOs.
- **Reproducibility** — a fixed `load.seed` reproduces pacing and sampling.

[Unreleased]: https://github.com/danny-oneal/redliner/compare/v0.5.1...HEAD
[0.5.1]: https://github.com/danny-oneal/redliner/compare/v0.5.0...v0.5.1
[0.5.0]: https://github.com/danny-oneal/redliner/compare/v0.4.1...v0.5.0
[0.4.1]: https://github.com/danny-oneal/redliner/compare/v0.4.0...v0.4.1
[0.4.0]: https://github.com/danny-oneal/redliner/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/danny-oneal/redliner/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/danny-oneal/redliner/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/danny-oneal/redliner/releases/tag/v0.1.0
