# Configuration reference

A Redliner run is driven by one YAML file (`--config`). CLI flags override
individual values but the file is the source of truth. Unknown keys are
rejected, so a typo fails the run before any traffic.

Durations use Go syntax: `250ms`, `2m`, `1h30m`, `0`.

The only hard requirements are `target.url` and at least one scenario (with
the field its protocol needs). Everything else has a default.

## Top level

```yaml
version: 1        # optional, informational
target:  { ... }
load:    { ... }
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
| `headers`    | map     | `{}`       | sent on every request |
| `auth`       | object  | `none`     | see below; credentials are read from the environment, never the file |
| `keep_alive` | bool    | `true`     | reuse pooled connections between requests |
| `max_idle_conns` | int | `10000`    | max idle pooled connections per host |
| `http2`      | bool    | `false`    | negotiate HTTP/2 with prior knowledge (cleartext h2c or a known h2 server) |

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
| `rate`     | float    | `200`     | requests per second (`constant`, `poisson`) |
| `duration` | duration | `30s`     | measured attack length (`constant`, `poisson`, `ramp`) |
| `warmup`   | duration | `0`       | unmeasured warmup at 10% of the first phase's rate |
| `ramp`     | object   | —         | `{ from, to }` rates for `profile: ramp` |
| `steps`    | list     | —         | `[{ rate, duration }, ...]` for `profile: steps` |
| `workers`  | int      | `10`      | initial concurrency (informational) |
| `max_workers` | int   | `100000`  | hard cap on in-flight requests |
| `seed`     | int      | `0`       | `0` derives a seed from the wall clock and records it; a fixed value reproduces pacing and sampling exactly |

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

```yaml
data:
  pools:
    ids:          { type: range, min: 1, max: 1000 }
    country_codes:{ type: list,  values: [US, CA] }
    skus:         { type: file,  path: skus.txt, as: string }
```

File paths resolve relative to the config file.

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

| key       | type   | notes |
| --------- | ------ | ----- |
| `name`    | string | used for per-operation report attribution |
| `weight`  | float  | relative selection weight |
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
| `headers` | map    | `{}`    | per-scenario, override the base headers for that route only |

**Templating.** A value that is *exactly* one placeholder keeps the sampled
value's native type (`id: "{{ids}}"` encodes as a JSON number); an embedded
placeholder (`"/u/{{ids}}/x"`) interpolates as a string.

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
enforced.

| key             | type     | notes |
| --------------- | -------- | ----- |
| `success_rate`  | float    | minimum fraction of successful requests (e.g. `0.995`) |
| `p50` / `p95` / `p99` | duration | maximum latency at that percentile |
| `throughput_min`| float    | minimum successful requests per second |

## `report`

Optional. Every run writes `report.dir/<UTC timestamp>/` and refreshes a
`latest` symlink; see the [README](../README.md#reports) for the file list.

| key      | type | default   | notes |
| -------- | ---- | --------- | ----- |
| `dir`    | string | `reports` | base directory for run folders (overridable with `--out`) |
| `junit`  | bool   | `false`   | also write `junit.xml` (SLO results as test cases) |
| `keep_raw` | bool | `true`    | also write `results.jsonl` (one JSON record per request) |

### Accepted for compatibility (no effect)

These keys are accepted so configs written for the original Go harness still
load, but they are not applied by the current Rust client:

- `target.read_buffer_bytes`, `target.write_buffer_bytes` — reqwest does not
  expose socket read/write buffer sizing.
- `report.per_operation` — per-operation reporting is always emitted when any
  operation is attributable.
