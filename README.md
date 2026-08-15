# capacity_bench

Vegeta-powered capacity/stress harness driven by a declarative YAML scenario
file. Speaks **GraphQL or REST**, fires a weighted mix of operations with
variables drawn from data pools (static or harvested from the live service),
paces load as constant / Poisson / ramp / stepped profiles, validates
responses at the application layer, and **gates CI on SLOs** with a non-zero
exit code.

## Layout

```
capacity_bench/
  main.go        orchestration: config → pools → phases → reports → SLO gate
  config.go      YAML schema, defaults, validation, flag overrides
  protocol.go    encoder interface + GraphQL/REST implementations
  targeter.go    weighted scenario mix + vegeta.Targeter
  pacer.go       PoissonPacer + load-profile → pacer factory
  data.go        pools (list/file/range), JSON-path extraction, templating
  bootstrap.go   harvest client (seeds pools from the live service)
  checks.go      app-layer response validation (status set, GraphQL errors)
  slo.go         SLO evaluation + JUnit XML
  report.go      collector, per-op/per-phase summaries, run manifest
  configs/       example scenario files (graphql-catalog, rest-example)
  configs/queries/  external .graphql documents referenced via query_file
```

## Quick start

```bash
make build

# GraphQL catalog server (reproduces the original harness behavior)
./capacity_bench -config configs/graphql-catalog.yaml

# REST API, stepped rates to find the throughput knee
./capacity_bench -config configs/rest-example.yaml

# CI: check a config without firing anything
./capacity_bench -config configs/rest-example.yaml -validate
```

Exit codes: `0` run passed, `1` SLO violated, `2` config/runtime error.

## Flags (overrides only — config is the source of truth)

| flag        | meaning                              |
| ----------- | ------------------------------------ |
| `-config`   | scenario file (default `capacity_bench.yaml`) |
| `-url`      | override `target.url`                |
| `-rate`     | override `load.rate`                 |
| `-duration` | override `load.duration`             |
| `-warmup`   | override `load.warmup`               |
| `-seed`     | override `load.seed`                 |
| `-out`      | override `report.dir`                |
| `-validate` | validate config and exit             |

## Scenario file

```yaml
target:
  url: http://localhost:8080/query   # full endpoint (graphql) or base URL (rest)
  protocol: graphql                  # graphql | rest
  timeout: 10s
  headers: { X-Team: platform }
  auth:                              # credentials come from env, never the file
    type: bearer                     # none | bearer | basic | header
    token_env: BENCH_TOKEN

load:
  profile: poisson                   # constant | poisson | ramp | steps
  rate: 200
  duration: 30s
  warmup: 3s                         # uniform at 10% rate, not measured
  # ramp:  { from: 50, to: 500 }     # profile: ramp
  # steps: [ { rate: 100, duration: 30s }, ... ]  # profile: steps
  seed: 0                            # 0 = time-based; set for reproducible runs

data:
  pools:                             # named value sources for {{placeholders}}
    ids:  { type: range, min: 1, max: 1000 }
    skus: { type: file, path: skus.txt }
    cc:   { type: list, values: [US, CA] }
  harvest:                           # seed pools from the live service
    - request: { query: "query { ... }" }        # graphql
    # - request: { method: GET, path: /products } # rest
      extract:
        skus: data.country.menus[].categories[].products[].sellableItems[].sku

scenarios:                           # weighted mix, drawn per hit
  - name: GetProduct
    weight: 50
    request:
      method: GET                    # rest fields: method/path/body/headers
      path: /products/{{ids}}
  - name: ProductQuery
    weight: 50
    request:
      query: "query($id:Int!){ product(id:$id){ name } }"   # graphql fields
      variables: { id: "{{ids}}" }   # whole-placeholder keeps native type
  - name: DeepQuery
    weight: 10
    request:
      query_file: queries/deep.graphql   # external .graphql for big selections
      variables: { id: "{{ids}}" }

checks:
  status: [200, 201]                 # allowed HTTP statuses (rest)
  graphql_errors: true               # 200-with-errors counts as failure

slos:                                # any violation → exit 1
  success_rate: 0.995
  p95: 250ms
  p99: 800ms
  throughput_min: 190

report:
  dir: reports                       # runs land in reports/<timestamp>/
  junit: true                        # junit.xml for CI test ingestion
```

Templating: `{{pool}}` anywhere in paths, bodies, or GraphQL variables. A
variable that is exactly one placeholder keeps the sampled value's native
type (`id: "{{ids}}"` stays an `Int`); embedded placeholders interpolate as
strings.

Queries: `query` (inline) and `query_file` (external `.graphql` document)
are mutually exclusive; use files for deeply nested selections. All file
paths — `query_file` and file pools — resolve relative to the config file,
not the working directory, so configs behave the same locally and in CI.

## Outputs

Each run writes `reports/<UTC timestamp>/` (plus a `latest` symlink):

- `text.txt`, `histogram.txt`, `metrics.json` — vegeta reports
- `per_operation.json` — p50/p95/p99 + success per scenario
- `per_phase.json` — per-step metrics (steps profile) for knee-finding
- `run_manifest.json` — full config, seed, SLO verdicts; the audit trail
- `junit.xml` — SLO results as test cases for CI
- `results.bin` — raw vegeta stream, replayable: `vegeta report -type=plot results.bin`

## CI usage

```yaml
# e.g. GitHub Actions
- run: ./capacity_bench -config configs/rest-example.yaml
# non-zero exit fails the job on SLO violation; junit.xml feeds test reports
```

## Design patterns

- **Strategy** — pacers (`constant|poisson|ramp|steps`) behind `vegeta.Pacer`;
  protocol encoders (`graphql|rest`) behind the `encoder` interface; pools
  (`list|file|range|harvested`) behind `Pool`.
- **Factory Method** — `newEncoder` and `buildPhases` map config onto strategies.
- **Adapter** — encoders adapt (scenario, sampled vars) onto `vegeta.Target`.
- **Decorator/interceptor** — `checker` reclassifies results between attacker
  and collector without touching either.
- **Bootstrap/probe** — harvest requests discover live IDs so report errors
  are real server errors, not synthetic 404s.
- **Self-correcting feedback** — `PoissonPacer` fires immediately when the
  observed hit count falls behind `rate × elapsed`.
- **Configuration object + overrides** — one validated `Config`; flags win
  only when explicitly set.

## Troubleshooting

- **`harvest: ... connection refused`** — service isn't up at `target.url`.
- **`pool "x": path ... matched nothing`** — the harvest extract path doesn't
  match the response shape; check it against the real JSON.
- **Actual rate well below target** — server saturation; check
  `per_phase.json` and non-2xx counts in `metrics.json`.
- **Success < 100% with all-200 statuses** — GraphQL resolver errors; see the
  `check: graphql errors` entries in the error list.
