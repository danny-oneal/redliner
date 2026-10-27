# Redliner

```
    R E D L I N E R
    ██████████████████████████████████▓▓▓▓▓▓▓◉
    SLO-driven HTTP load testing — push it to the red line
```

[![CI](https://github.com/danny-oneal/redliner/actions/workflows/ci.yml/badge.svg)](https://github.com/danny-oneal/redliner/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

**SLO-driven HTTP load testing in Rust.** Redliner fires a declarative,
weighted mix of **GraphQL or REST** operations at a target, paces the load as
a constant / Poisson / ramp / stepped profile, draws request values from data
pools (static or **harvested from the live service** before the run), validates
responses at the application layer, and **gates CI on SLOs** with a non-zero
exit code. Every run leaves a timestamped report directory for audit and
dashboards.

It is a from-scratch Rust port of an internal Go/Vegeta harness, built as a
Cargo workspace: a reusable async load engine (`volley`, packaged as `redliner-volley`) and the configurable
binary (`redliner`).

## Features

- **One YAML file** describes the target, auth, load profile, operation mix,
  data pools, checks, SLOs, and reports. Unknown keys are rejected, so typos
  fail fast.
- **GraphQL and REST** behind one encoder seam — switch with `target.protocol`.
- **Pacing profiles:** `constant`, `poisson` (seeded, self-correcting),
  `ramp`, and `steps` (to find the throughput knee).
- **Data pools & templating:** `{{pool}}` placeholders in paths, bodies, and
  GraphQL variables, sampled from `list` / `range` / `file` pools or values
  **harvested** from the live service by JSON path.
- **App-layer checks:** reclassify a `200`-with-GraphQL-`errors` or an
  out-of-set status as a failure, so the success rate tells the truth.
- **SLO gate:** success rate, p50/p95/p99, and throughput thresholds decide the
  process exit code.
- **Reports:** per-run `reports/<timestamp>/` with summaries, a latency
  histogram, `metrics.json`, per-operation / per-phase breakdowns, a JUnit file
  for CI, a raw `results.jsonl` stream, and a full run manifest.
- **Production-log replay:** feed a JSONL or HAR request log and Redliner fires
  the matching scenarios in the log's order and at its pace, with `--speed` and
  `--copies` to compress or amplify, still gated by the checks and SLOs.
- **Reproducible:** a fixed `load.seed` reproduces pacing and sampling exactly.

## Install

**Prebuilt binary** (Linux x86_64/arm64, macOS). The script picks the release
asset for your platform, checks it against the release's SHA-256 checksum, and
installs it to `~/.local/bin`:

```bash
curl -fsSL https://raw.githubusercontent.com/danny-oneal/redliner/main/scripts/install.sh | sh
```

Set `REDLINER_VERSION=v0.2.0` to pin a release or `REDLINER_INSTALL_DIR` to
install elsewhere. The Linux binaries are static (musl), so they run on any
distro and in `scratch`/distroless images. Windows builds are attached to each
[release](https://github.com/danny-oneal/redliner/releases) as a `.zip` with a
`.sha256`.

**From source** needs a **Rust toolchain ≥ 1.88** ([rustup](https://rustup.rs)).

```bash
# From a local checkout (also: `make install`):
cargo install --path crates/redliner --locked

# Or straight from GitHub:
cargo install --git https://github.com/danny-oneal/redliner redliner --locked
```

Both drop a `redliner` binary in `~/.cargo/bin`. To run from source without
installing, use `cargo run -p redliner -- <args>`.

## Quick start

```bash
# GraphQL catalog server
redliner --config configs/graphql-catalog.yaml

# REST API, stepped rates to find the throughput knee
redliner --config configs/rest-example.yaml

# CI: validate a config without firing any traffic
redliner --config configs/rest-example.yaml --validate

# Override a few knobs without editing the file
redliner --config configs/rest-example.yaml --rate 500 --duration 2m

# Replay a production log at 2x speed (see "Production-log replay" below)
redliner --config configs/replay-example.yaml --replay prod.jsonl --speed 2
```

**Exit codes:** `0` run passed · `1` an SLO was violated · `2` config or
runtime error · `130` interrupted.

**Ctrl-C** stops a run early without losing it: no new requests are fired,
in-flight ones finish (up to `target.timeout`), and the report directory is
written from the partial results, with `"aborted": true` in
`run_manifest.json`. The run exits `130`. A second Ctrl-C quits at once,
without reports.

**Live stats.** While a run is going, a progress line goes to stderr every 10s
(`report.live_stats` / `--live-stats`; `0s` turns it off). It shows the running
request total, plus the rate, success, p95 and failure count of the last
window, so a run that is going bad is visible before the summary:

```
[live 20.0s] step-2(200 rps) · 3012 requests · last 10.0s: 199.8 rps, 99.95% ok, p95 12.4ms, 1 failed
```

## How it works

```
config (YAML) ─▶ build pools ─▶ harvest probe ─▶ weighted mix + encoder
                                                        │
                                                        ▼
            per-phase paced attack (volley engine) ─▶ app-layer checks
                                                        │
                                                        ▼
                         collector ─▶ SLO gate ─▶ report dir + exit code
```

The workspace has two crates:

| crate        | role |
| ------------ | ---- |
| **`volley`** | protocol-agnostic async load engine: `Target`/`AttackResult`/`Metrics`, the `Pacer` and `Targeter` traits, pacers, and the tokio `Attacker` that fires at a paced rate over reqwest. Depends on nothing in `redliner`. |
| **`redliner`** | the binary: YAML config model + validation, CLI overrides, GraphQL/REST encoders, data pools + harvest, app-layer checks, SLO gate, and the report directory. |

## CLI

The config file is the source of truth; flags override only what you set.

| flag          | overrides      | notes                                   |
| ------------- | -------------- | --------------------------------------- |
| `--config`    | —              | scenario file (default `redliner.yaml`) |
| `--url`       | `target.url`   |                                         |
| `--rate`      | `load.rate`    | requests per second                     |
| `--duration`  | `load.duration`| Go-style duration, e.g. `30s`, `2m`     |
| `--warmup`    | `load.warmup`  | Go-style duration                       |
| `--seed`      | `load.seed`    | fix for a reproducible run              |
| `--out`       | `report.dir`   |                                         |
| `--live-stats`| `report.live_stats` | progress line interval, e.g. `5s`; `0s` turns it off |
| `--validate`  | —              | validate the config and exit `0`        |
| `--replay`    | `replay.source`| replay a JSONL/HAR log instead of a load profile |
| `--speed`     | `replay.speed` | replay time compression (needs replay)  |
| `--copies`    | `replay.copies`| concurrent copies of the log (needs replay) |
| `--strict`    | `replay.on_unmatched` | fail instead of skipping unmatched entries (needs replay) |

## Configuration

A minimal config is a target URL and one scenario; everything else has a
default. See **[docs/configuration.md](docs/configuration.md)** for the full
schema, and [`configs/`](configs/) for worked GraphQL and REST examples.

```yaml
target:
  url: http://localhost:8080/query   # full endpoint (graphql) or base URL (rest)
  protocol: graphql                  # graphql | rest
  timeout: 10s
  auth:                              # credentials come from env, never the file
    type: bearer                     # none | bearer | basic | header
    token_env: BENCH_TOKEN

load:
  profile: poisson                   # constant | poisson | ramp | steps
  rate: 200
  duration: 30s
  warmup: 3s                         # uniform at 10% rate, not measured
  seed: 0                            # 0 = time-based; set for reproducible runs

data:
  pools:
    ids: { type: range, min: 1, max: 1000 }
  harvest:                           # seed pools from the live service first
    - request: { query: "query{ products{ id } }" }
      extract: { ids: data.products[].id }

scenarios:                           # weighted mix, one drawn per request
  - name: GetProduct
    weight: 1
    request:
      query: "query($id:Int!){ product(id:$id){ name } }"
      variables: { id: "{{ids}}" }   # a whole-placeholder keeps its native type

checks:
  graphql_errors: true               # a 200 carrying `errors` counts as a failure

slos:                                # any violation → exit 1
  success_rate: 0.995
  p95: 250ms

report:
  dir: reports
  junit: true
```

**Templating:** `{{pool}}` works anywhere in a path, body, or GraphQL
variable. A value that is *exactly* one placeholder keeps the sampled value's
native type (`id: "{{ids}}"` stays an integer); an embedded placeholder
(`/u/{{ids}}/x`) interpolates as a string. All file paths (`query_file`, file
pools) resolve relative to the config file, so a config behaves the same
locally and in CI.

## Production-log replay

Instead of a synthetic profile, replay what production actually saw. Give each
scenario a `match` rule and point `replay.source` (or `--replay`) at a JSONL or
HAR log; Redliner fires the matched operations in the log's order and at its
inter-arrival pace. Request *values* still come from your data pools, not the
log.

```yaml
replay:
  source: prod.jsonl      # one JSON object per line: ts, method, path, operation
  speed: 2.0              # 2x faster than recorded
  copies: 3               # 3x the volume over the same duration

scenarios:
  - name: GetProduct
    weight: 1                            # profile mode only; replay ignores it
    match: { operation: GetProduct }     # REST: { method: GET, path: /products/* }
    request:
      query: "query GetProduct($id:Int!){ product(id:$id){ name } }"
      variables: { id: "{{ids}}" }
```

`weight` is ignored in replay mode and `match` in profile mode, so one file
serves both. Unmatched entries are skipped (or fail the run with `--strict`);
malformed lines are skipped and counted. `redliner --validate` previews the
match counts without firing traffic. See
[docs/configuration.md](docs/configuration.md#replay) and
[`configs/replay-example.yaml`](configs/replay-example.yaml).

## Reports

Each run writes `reports/<UTC timestamp>/` and refreshes a `latest` symlink:

| file                  | contents |
| --------------------- | -------- |
| `text.txt`            | human-readable overall summary |
| `histogram.txt`       | latency histogram over log-spaced buckets |
| `metrics.json`        | overall metrics (rate, throughput, success, latencies, status codes) |
| `per_operation.json`  | requests / success / p50·p95·p99 per scenario |
| `per_phase.json`      | per-phase metrics (written when more than one phase ran) |
| `run_manifest.json`   | full config, seed, and SLO verdicts — the audit trail |
| `junit.xml`           | SLO results as test cases (when `report.junit: true`) |
| `results.jsonl`       | raw per-request stream, one JSON object per line (when `report.keep_raw: true`) |

When a run violates an SLO, the summary also prints a redline bar so a failure
is impossible to miss in a scrollback or CI log.

## CI usage

```yaml
# GitHub Actions
- run: redliner --config configs/rest-example.yaml
  # a non-zero exit fails the job on an SLO violation;
  # upload reports/latest/junit.xml to surface SLOs as test results
```

## Run in K3s

Package Redliner as a container image and run it as a one-shot Kubernetes Job
to load-test an in-cluster service. See **[deploy/k3s/README.md](deploy/k3s/README.md)**
for the build → `k3s ctr images import` → `kubectl apply` runbook.

## Development

```bash
make test     # cargo test --workspace
make lint     # cargo clippy --workspace --all-targets -- -D warnings
make build    # release binary
make run CONFIG=configs/rest-example.yaml
```

See **[CONTRIBUTING.md](CONTRIBUTING.md)** for the workflow and conventions,
and **[CHANGELOG.md](CHANGELOG.md)** for release notes.

## License

[MIT](LICENSE) © Danny O'Neal
