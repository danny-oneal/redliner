# capacity_bench — corporate-readiness changes

Goal: turn a single-purpose gqlgen stress script into a harness a team can
share: protocol-configurable (GraphQL **or** REST), config-driven, CI-gated,
and auditable. All prior behavior is preserved via
`configs/graphql-catalog.yaml`.

## 1. Declarative YAML scenario files (`config.go`, `configs/`)

**What:** Everything — target, auth, load profile, query/endpoint mix, data
pools, checks, SLOs — now lives in a validated YAML file. Flags only
override (`-rate`, `-duration`, `-url`, `-seed`, `-out`); `-validate` checks
a config without firing. Unknown keys are rejected, so typos fail fast.

**Why:** Corporate load tests must be reviewable (config in git, changes go
through PRs), repeatable (config + seed = same run), and shareable across
services without recompiling. Hard-coded queries made the old harness
single-purpose.

## 2. Protocol abstraction: GraphQL or REST (`protocol.go`)

**What:** A small `encoder` interface (Strategy pattern) with two
implementations. GraphQL scenarios declare `query` + `variables`; REST
scenarios declare `method`/`path`/`body`/`headers`. Everything upstream —
mix, pacing, attacker, reports — is protocol-agnostic. Switching is one
config line: `target.protocol: rest`.

**Why:** Your requirement. The protocol difference is isolated to one file,
so a third protocol (gRPC-web, JSON-RPC) is one new encoder, not a rewrite.

## 3. Generic data pools + harvest (`data.go`, `bootstrap.go`)

**What:** Named pools (`list`, `file`, `range`) feed `{{placeholder}}`
templates in paths, bodies, and GraphQL variables. The old
catalog-specific corpus bootstrap became a generic `harvest` step: any
GraphQL/REST request + JSON paths (`data.menus[].products[].id`) that seed
pools from the live service. Whole-placeholder variables keep native types
(Int stays Int).

**Why:** The corpus logic was hard-wired to one schema. Teams now point the
same binary at any API and still get the property that every request
resolves to real data — so report errors are genuine server errors.

## 4. Load profiles (`pacer.go`)

**What:** `load.profile: constant | poisson | ramp | steps`. Poisson (the
original behavior) is kept as default; `ramp` sweeps between two rates;
`steps` runs discrete rate plateaus with per-phase metrics for finding the
throughput knee. Each phase gets a fresh attacker (preserves the known
vegeta reuse quirk fix).

**Why:** Different questions need different shapes: SLO validation at steady
state (poisson/constant), capacity discovery (steps/ramp). Previously this
required code edits.

## 5. Application-layer checks (`checks.go`)

**What:** A `checker` reclassifies results after the HTTP layer: an
allowed-status list for REST, and — critical for GraphQL — 200 responses
whose body carries a top-level `errors` array are counted as failures, with
the first error message surfaced in the report.

**Why:** GraphQL servers return 200 for resolver errors. The old harness
would report 100% success while every response was an error payload. That's
the kind of false-green that burns teams in production.

## 6. SLO gate for CI (`slo.go`)

**What:** Optional `slos:` block (success rate, p50/p95/p99, min
throughput). Violations print a PASS/FAIL table, are written to
`junit.xml`, and exit `1` (config errors exit `2`). Enforced only when SLOs
are declared.

**Why:** A load test that can't fail a pipeline is a dashboard, not a gate.
JUnit output makes SLO verdicts show up as test results in Jenkins/GitHub
Actions/GitLab with zero extra tooling.

## 7. Auth + custom headers (`protocol.go`)

**What:** `target.auth` supports bearer, basic, and custom-header API keys —
resolved from environment variables only, never stored in config. Static
headers (e.g. `X-Team`) apply to every request including harvest.

**Why:** Corporate APIs sit behind auth; secrets in config files fail
security review. Missing env vars fail fast at startup, not mid-run.

## 8. Audit-grade reporting (`report.go`)

**What:** Runs write to `reports/<UTC timestamp>/` with a `latest` symlink
(history preserved). New artifacts: `run_manifest.json` (full resolved
config, seed, SLO verdicts — written at start so aborted runs leave a
trail), `per_operation.json` (latency/success per scenario), and
`per_phase.json` (per-step, for knee-finding). GraphQL requests carry an
`?op=Name` tag so per-scenario attribution also works in server access logs.

**Why:** "What exactly did we run last Tuesday and what did it show?" should
be answerable from artifacts alone. Overwriting reports destroyed history.

## 9. Tests, Makefile, hygiene (`*_test.go`, `Makefile`, `.gitignore`)

**What:** Unit tests for config validation, templating/type preservation,
JSON-path extraction, weighted mix distribution, both encoders (including
result→scenario attribution), checks, and SLO evaluation. `make
build/test/vet/validate/run`. `.gitignore` for the binary, `reports/`, and
`trace.out` (currently committed — worth `git rm --cached`ing). `go.mod`'s
Go directive lowered from `1.26.4` to `1.23.0` — the true minimum
(vegeta needs 1.22), so CI images and teammates aren't forced onto a
bleeding-edge toolchain.

**Why:** A shared tool needs tests so contributions don't regress it, and a
one-word build/test entry point.

## 10. External `.graphql` query files (`query_file`)

**What:** GraphQL scenarios (and harvest requests) accept `query_file:
queries/country-full.graphql` as an alternative to inline `query:` — the two
are mutually exclusive. Paths resolve relative to the config file (file
pools too), so configs behave identically from CI or any working directory.
The deep `CountryFull` query in the example config now lives in
`configs/queries/country-full.graphql`.

**Why:** Deeply nested selection sets are unreadable inside YAML block
scalars and get no editor/tooling support. `.graphql` files bring syntax
highlighting, graphql linting/formatting, and let teams load-test the exact
operation documents their clients ship. Inline stays available because
forcing files on three-line smoke queries is needless friction.

## Verified

Built and unit-tested (`go vet`, `go test`), plus end-to-end smoke runs
against stub servers: GraphQL poisson run (per-op mix ≈ configured weights,
SLOs pass, exit 0), GraphQL with 20% injected resolver errors (success
drops to ~81%, error surfaced, junit failure, exit 1), REST steps profile
(harvest, path templating, POST bodies, per-phase report, throughput SLO
fail → exit 1).

## Alternatives considered (ask for detail if interested)

- k6 / Gatling / Locust instead of vegeta (richer ecosystems, heavier adoption cost)
- Go `text/template` or CEL for templating (more power than needed here)
- JSONPath/GJSON dependency for extraction (kept a ~60-line subset instead)
- Custom `http.RoundTripper` for checks (vs. the collector-side interceptor)
- Per-scenario SLOs, latency-degradation autostop, Prometheus remote-write
  (natural next steps if needed)
