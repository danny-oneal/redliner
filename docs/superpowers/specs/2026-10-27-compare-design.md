# Run Comparison (`redliner compare`) — Design

**Date:** 2026-10-27
**Status:** Implemented. The maintainer asked for sensible defaults; the
decisions below are the implementer's calls and can be revisited.
**Topic:** Compare two finished runs and fail CI when the candidate regressed
against the baseline.

## Intent

The SLO gate checks absolute limits ("p95 under 200ms"). That misses gradual
drift: a service can get 30% slower and still be under its SLO. `compare`
adds a relative gate: "no worse than the baseline by more than N%". Goku's
`compare` prints a table but can't fail a build.

## Goals

- `redliner compare <baseline> <candidate>` prints the change in p50, p95,
  p99, throughput and success rate, overall, per load phase and per
  operation.
- `--max-regression <PERCENT>` turns the report into a gate: exit `1` on a
  regression, `0` otherwise, `2` when a report can't be read.

## Non-goals

- Storing or fetching baselines. Where the baseline comes from (a committed
  report, a CI cache or artifact from main) is the user's pipeline's job.
- Statistical significance testing. The reports hold percentiles, not
  samples, so a confidence interval isn't available without `results.jsonl`.
- JSON output. This can be added later if something needs to consume the
  comparison in code.

## Decisions

- **Inputs:** a report directory (including the `latest` symlink) or its
  `metrics.json`. `per_phase.json` and `per_operation.json` next to it are
  read when present.
  Unknown fields are ignored, so older baselines keep working as reports
  grow.
- **Gated metrics:**
  - p50/p95/p99 regress when they rise by more than the threshold, measured
    against the baseline.
  - Throughput regresses when it falls by more than the threshold.
  - Changes equal to the threshold pass, so identical runs pass `0%`.
- **Success rate is reported, not gated.** It's shown in percentage points,
  because relative change on a value near 100% is meaningless. Its floor
  belongs to `slos.success_rate`, which every run already enforces.
- **Per-operation rows are gated too.** That's where a regression in one
  endpoint shows up, before it moves the overall numbers. Operations present
  in only one run are listed, not compared.
- **Per-phase rows are gated too** (added after the first version, at the
  maintainer's request). Only `steps` runs write `per_phase.json`, and that's
  where they matter: a regression near the throughput knee can double one
  step's p99 while the run's overall p99 stays under the threshold. Phases
  are matched by name (`step-3(200 rps)`), which includes the rate, so they
  match only when both runs used the same steps. The others are listed as
  "only in" the run that has them, with a `phase ` prefix. Phase rows have
  no throughput, because `per_phase.json` doesn't record it.
- **A zero baseline** has no percent change (`n/a`) and isn't gated.
- **Different rates warn.** If the two runs fired at rates more than 5%
  apart, the comparison isn't like for like; compare warns on stderr rather
  than failing.
- **One threshold for everything.** Separate thresholds per metric are
  possible later. One flag keeps the common case to one line.
- **CLI shape:** a clap subcommand on the existing parser
  (`args_conflicts_with_subcommands`), so `redliner --url …` and every run
  flag are unchanged. Run flags given together with `compare` are rejected.
- **Exit codes** reuse the run's: `1` (gate failed) and `2` (error).

## Known limitations

- Small latencies are noisy in relative terms (400µs → 480µs is +20%). The
  docs tell users to pick a threshold to suit and to compare runs made with
  the same config and rate. A noise floor (ignore changes under N ms) is the
  obvious follow-up if this bites.
