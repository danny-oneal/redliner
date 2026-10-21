# Dockerize Red Liner for K3s — Design

**Date:** 2026-10-20
**Status:** Approved direction; spec under review
**Topic:** Package the `red-liner` CLI as a container image and run it as a
one-shot Kubernetes Job on K3s to load-test a target service.

## Intent

Red Liner is a run-to-completion batch CLI: it reads a YAML scenario, fires a
paced load at a target, writes a `reports/<timestamp>/` directory, and exits
`0` (passed), `1` (SLO violated), or `2` (config/runtime error). Today it only
runs from a local Rust toolchain or a `cargo install` binary.

The goal is to make it **easy to run Red Liner inside K3s as a containerized
workload** so a scenario can be fired at an in-cluster service (or external
URL) and the exit code gates pass/fail. Success means: a maintainer can build
an image, import it into a local (single-node) K3s, `kubectl apply` a Job, and
read the result from the exit code and pod logs — with no registry and no
persistent storage required.

### Decisions captured from brainstorming

- **Workload:** one-shot Kubernetes **Job** (not Deployment/CronJob).
- **Reports:** **not persisted.** Rely on exit code, the human-readable text
  summary, and the JUnit XML — all surfaced to pod logs. No PVC.
- **Image delivery:** **import into containerd** via `k3s ctr images import`
  (single-node/local K3s). No registry; `imagePullPolicy: Never`.

## Non-goals (YAGNI)

These are explicitly out of scope and left as documented extension points:

- PersistentVolume / report persistence beyond logs.
- CronJob or scheduled runs.
- Long-running Deployment / sidecar loop.
- Helm chart or Kustomize overlays.
- Registry push, multi-node pull, multi-arch (arm64) builds.
- Any change to Red Liner's Rust source. This work adds packaging and
  deployment artifacts only; the binary's behavior is unchanged.

## Architecture

```
Dockerfile (multi-stage)                deploy/k3s/
  ┌─────────────────────────┐          ┌───────────────────────────────┐
  │ chef   : cargo-chef plan│          │ configmap.yaml → scenario.yaml │
  │ builder: cargo build     │          │ secret.example.yaml → token    │
  │          --release       │          │ job.yaml → runs the image      │
  │ runtime: distroless/cc   │◀── image │                                │
  │          :nonroot        │  import  │  mounts ConfigMap at           │
  └─────────────────────────┘   via    │  /etc/red-liner, env from      │
          │                   k3s ctr   │  Secret, --out /tmp/reports    │
          ▼                             └───────────────────────────────┘
   red-liner binary                              │ kubectl apply
   ENTRYPOINT ["/usr/local/bin/red-liner"]       ▼
                                          Job pod: run → log summary +
                                          JUnit → exit 0/1/2
```

### Component 1 — Dockerfile (multi-stage, cargo-chef, distroless)

A three-stage build keyed to the project's pinned toolchain:

1. **chef** — base `rust:1.88-slim-bookworm` with `cargo-chef` installed;
   produces a dependency `recipe.json`.
2. **builder** — `cargo chef cook --release` to compile dependencies as a
   cached layer, then copies the real source and runs
   `cargo build --release -p red-liner --locked`. Debian bookworm is glibc and
   matches the `-debian12` runtime base below.
3. **runtime** — `gcr.io/distroless/cc-debian12:nonroot`. Copies only the
   `red-liner` binary. Distroless/cc provides glibc and CA certificates but no
   shell or package manager; `:nonroot` runs as uid 65532.

Rationale:

- `reqwest` is built with `rustls-tls` (default features off), so TLS is
  pure-Rust with bundled Mozilla roots — no OpenSSL, no system TLS libs. The
  binary needs only glibc + (optionally) CA certs, both present on
  distroless/cc.
- Pin the builder to `1.88` (matching `Cargo.toml` `rust-version` and
  `rust-toolchain.toml`'s intent) rather than floating `stable`, for
  reproducible images. If the floating stable toolchain is preferred, the tag
  becomes `rust:slim-bookworm`; the spec pins `1.88` by default.
- `ENTRYPOINT ["/usr/local/bin/red-liner"]` with no `CMD`, so Job args append
  cleanly (`args: ["--config", "/etc/red-liner/scenario.yaml"]`).
- Build with `--locked` to honor `Cargo.lock`.

Smaller alternative (documented, not built): a `x86_64-unknown-linux-musl`
static build onto `scratch` (~15 MB). Deferred because reqwest's rustls stack
pulls `aws-lc-rs`, which needs cmake and a musl C toolchain — real build
friction for a marginal size win.

### Component 2 — `.dockerignore`

Exclude `target/`, `reports/`, `.git/`, `docs/`, and local K8s secrets from the
build context so builds are fast and never ship artifacts or credentials.

### Component 3 — `deploy/k3s/` manifests

- **configmap.yaml** — a `ConfigMap` named `red-liner-scenario` holding
  `scenario.yaml`: a worked example pointing at an in-cluster Service
  (e.g. `http://echo.default.svc.cluster.local/`) with a short, low-rate load
  profile and a couple of SLOs. The scenario is self-contained (no
  `query_file`/file-pool references) so a single-file ConfigMap mount suffices;
  a note documents that referenced files would need to be added as extra
  ConfigMap keys mounted into the same directory.
- **secret.example.yaml** — a template `Secret` (`stringData: BENCH_TOKEN: ""`)
  the user copies to `secret.yaml` and fills in. The real `secret.yaml` is
  git-ignored. The scenario only references a token via `auth.token_env`, so
  the example scenario defaults to `auth.type: none` and the Secret/env wiring
  is shown commented in `job.yaml` for when auth is needed.
- **job.yaml** — the `Job`:
  - `spec.backoffLimit: 0`, `template.spec.restartPolicy: Never` — a load test
    runs once; don't retry on SLO failure.
  - container `image: red-liner:dev`, `imagePullPolicy: Never`.
  - `args: ["--config", "/etc/red-liner/scenario.yaml", "--out", "/tmp/reports"]`.
  - volumeMounts: ConfigMap at `/etc/red-liner` (read-only); `emptyDir` at
    `/tmp` (writable report scratch under a read-only root fs).
  - `securityContext`: `runAsNonRoot: true`, `readOnlyRootFilesystem: true`,
    `allowPrivilegeEscalation: false`, `capabilities: { drop: [ALL] }`,
    `seccompProfile: { type: RuntimeDefault }`.
  - `resources`: modest requests/limits (e.g. 100m/500m CPU, 64Mi/256Mi mem).
  - commented-out `envFrom`/`env` block wiring `BENCH_TOKEN` from the Secret.

### Component 4 — Makefile targets

Add to the existing `Makefile`:

- `docker-build` — `docker build -t red-liner:dev .`
- `image-save` — `docker save red-liner:dev -o red-liner-dev.tar` (tar
  git-ignored).
- `k3s-import` — `sudo k3s ctr images import red-liner-dev.tar`.
- `k3s-deploy` — `kubectl apply -f deploy/k3s/configmap.yaml -f deploy/k3s/job.yaml`.
- `k3s-logs` — convenience: `kubectl logs -f job/red-liner`.

(These are additive; existing `build`/`test`/`lint`/etc. are untouched.)

### Component 5 — `deploy/k3s/README.md`

A short runbook: prerequisites (Docker, a running K3s), the build → save →
`k3s ctr images import` → `kubectl apply` → read-result flow, how to point the
scenario at a real target, how to add an auth token via the Secret, and the
documented extension points (PVC, registry, musl/scratch).

## Data flow

1. `docker build` compiles `red-liner` and bakes it into a distroless image.
2. `docker save` + `k3s ctr images import` places the image in the node's
   containerd store.
3. `kubectl apply` creates the ConfigMap and the Job.
4. The Job pod starts, reads `/etc/red-liner/scenario.yaml`, fires load at the
   target, writes a report under `/tmp/reports`, prints the text summary and
   (if `report.junit: true`) echoes the JUnit XML to stdout, then exits.
5. The pod's exit code sets the Job's success/failure; `kubectl get job` and
   `kubectl logs` surface the result. The `/tmp` report dir dies with the pod.

Note: the current binary writes report files to disk and prints a summary to
stdout. If JUnit content is not already echoed to stdout, the README documents
reading it via `kubectl logs` of the summary and treating the exit code as the
gate; no source change is made to force extra stdout output (that would be a
separate, out-of-scope enhancement).

## Error handling

- **Build failure** (compile error, lockfile drift) → `docker build` fails
  loudly; `--locked` surfaces lockfile mismatches.
- **Image not found in K3s** → `imagePullPolicy: Never` means a missing import
  yields `ErrImageNeverPull`; README calls this out as the signal to re-run
  `k3s-import`.
- **Config error** (bad scenario) → binary exits `2`; Job is marked failed; the
  reason is in pod logs.
- **SLO violation** → binary exits `1`; Job failed with a clean report in logs
  — the intended CI-style gate.
- **Read-only fs write attempt** outside `/tmp` → prevented by design; the only
  writable path is the `emptyDir` at `/tmp`, and `--out /tmp/reports` targets
  it.

## Testing / verification

Manual, since this is packaging (no unit-testable Rust added):

1. `make docker-build` succeeds and produces `red-liner:dev`.
2. `docker run --rm red-liner:dev --help` prints usage (ENTRYPOINT wired).
3. `docker run --rm -v $PWD/configs:/etc/red-liner red-liner:dev --config
   /etc/red-liner/rest-example.yaml --validate` exits `0` (binary runs,
   config parses, nonroot can read a mounted config).
4. In K3s: import the image, deploy a trivial in-cluster echo target
   (documented one-liner, e.g. `kubectl create deployment echo
   --image=... && kubectl expose ...`), `kubectl apply` the Job, then
   `kubectl wait --for=condition=complete job/red-liner` and confirm the exit
   path via `kubectl logs`.
5. Confirm image size is in the expected ~25–35 MB range.

A CI lint of the YAML (e.g. `kubectl apply --dry-run=client`) is noted as an
optional follow-up, not required here.

## Files added/changed

| Path | Change |
|---|---|
| `Dockerfile` | new — multi-stage cargo-chef + distroless/cc build |
| `.dockerignore` | new — exclude target/reports/.git/docs/secrets |
| `deploy/k3s/configmap.yaml` | new — example scenario ConfigMap |
| `deploy/k3s/secret.example.yaml` | new — token Secret template |
| `deploy/k3s/job.yaml` | new — one-shot hardened Job |
| `deploy/k3s/README.md` | new — build + deploy runbook |
| `Makefile` | edit — add docker/k3s targets |
| `.gitignore` | edit — ignore `red-liner-dev.tar`, `deploy/k3s/secret.yaml` |
| `README.md` | edit (small) — add a "Run in K3s" pointer to the runbook |
