# Dockerize Red Liner for K3s — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Package the `red-liner` CLI as a minimal container image and run it as a one-shot Kubernetes Job on a local (single-node) K3s to load-test a target service.

**Architecture:** A multi-stage Dockerfile (cargo-chef dependency caching → distroless/cc runtime) produces a small, nonroot image. K3s manifests under `deploy/k3s/` run it as a hardened Job that mounts its scenario from a ConfigMap, reads an optional auth token from a Secret, writes reports to an ephemeral `/tmp`, and surfaces the result through the exit code and pod logs. No Rust source changes.

**Tech Stack:** Rust 1.88, Docker (multi-stage, cargo-chef), `gcr.io/distroless/cc-debian12:nonroot`, Kubernetes Job/ConfigMap/Secret, K3s containerd (`k3s ctr images import`), Make.

**Spec:** `docs/superpowers/specs/2026-10-20-dockerize-k3s-design.md`

## Global Constraints

- Rust builder pinned to `1.88` (matches `Cargo.toml` `rust-version`); build with `--locked`.
- No changes to any file under `crates/` — packaging and deployment artifacts only.
- Runtime base: `gcr.io/distroless/cc-debian12:nonroot` (glibc + CA certs, no shell, uid 65532).
- Image tag for local use: `red-liner:dev`; `imagePullPolicy: Never`.
- Auth tokens never appear in the image, Dockerfile, or committed ConfigMap/Secret — only in a git-ignored `deploy/k3s/secret.yaml`.
- Reports are not persisted; the only writable path in the Job is an `emptyDir` at `/tmp`, and `--out /tmp/reports` targets it.
- Job: `restartPolicy: Never`, `backoffLimit: 0`.

## Review Focus

- **Read-only root filesystem + report writes:** the binary writes a report dir; under `readOnlyRootFilesystem: true` only `/tmp` is writable. Covered by Task 2's `--out /tmp/reports` and the Task 1 RO-fs run check.
- **Nonroot reads a ConfigMap-mounted config:** uid 65532 must be able to read `/etc/red-liner/scenario.yaml` (ConfigMap default mode 0644 is world-readable). Covered by Task 1's nonroot `--validate` run against a mounted config.
- **ENTRYPOINT argument passthrough:** Job `args:` must append to the binary, not replace a shell. Covered by Task 1's `docker run ... --validate` (args reach the binary).
- **Missing imported image:** `imagePullPolicy: Never` yields `ErrImageNeverPull` if the import step was skipped — documented as the signal in Task 3's README, not a test.
- **HTTPS target TLS from distroless:** reqwest's bundled rustls roots mean no system certs are required; exercising a real HTTPS target is left to the operator and documented in Task 3's README, not asserted here.

---

### Task 1: Container image

**Files:**
- Create: `Dockerfile`
- Create: `.dockerignore`

**Interfaces:**
- Produces: an image `red-liner:dev` with `ENTRYPOINT ["/usr/local/bin/red-liner"]` and no `CMD`, runnable as `docker run --rm red-liner:dev <red-liner args>`.

- [ ] **Step 1: Write `.dockerignore`**

Exclude from the build context: `target/`, `reports/`, `.git/`, `docs/`, `deploy/`, `*.tar`.

- [ ] **Step 2: Write `Dockerfile` (three stages)**

- `chef` stage: `FROM rust:1.88-slim-bookworm AS chef`, `RUN cargo install cargo-chef --locked`, workdir `/app`.
- `planner` stage: `FROM chef AS planner`, copy the source, `RUN cargo chef prepare --recipe-path recipe.json`.
- `builder` stage: `FROM chef AS builder`, copy `--from=planner /app/recipe.json`, `RUN cargo chef cook --release --recipe-path recipe.json`, copy the source, `RUN cargo build --release -p red-liner --locked`.
- runtime stage: `FROM gcr.io/distroless/cc-debian12:nonroot`, `COPY --from=builder /app/target/release/red-liner /usr/local/bin/red-liner`, `ENTRYPOINT ["/usr/local/bin/red-liner"]`.

- [ ] **Step 3: Build the image**

Run: `docker build -t red-liner:dev .`
Expected: build succeeds; final image is a distroless runtime (no toolchain).

- [ ] **Step 4: Verify ENTRYPOINT arg passthrough and usage**

Run: `docker run --rm red-liner:dev --help`
Expected: prints `red-liner` usage/flags and exits 0 (args reached the binary).

- [ ] **Step 5: Verify nonroot read of a mounted config under the real binary**

Run: `docker run --rm -v "$PWD/configs:/etc/red-liner:ro" --read-only --tmpfs /tmp red-liner:dev --config /etc/red-liner/rest-example.yaml --validate`
Expected: exits 0 (nonroot reads the mounted config; `--validate` parses it; read-only rootfs with a tmpfs `/tmp` is tolerated).

- [ ] **Step 6: Check image size is in range**

Run: `docker images red-liner:dev --format '{{.Size}}'`
Expected: roughly 25–35 MB (sanity check that the runtime is distroless, not the builder).

- [ ] **Step 7: Commit**

```bash
git add Dockerfile .dockerignore
git commit -m "feat: multi-stage Dockerfile for red-liner (distroless, nonroot)"
```

---

### Task 2: K3s manifests

**Files:**
- Create: `deploy/k3s/configmap.yaml`
- Create: `deploy/k3s/secret.example.yaml`
- Create: `deploy/k3s/job.yaml`

**Interfaces:**
- Consumes: the `red-liner:dev` image from Task 1.
- Produces: a ConfigMap `red-liner-scenario` (key `scenario.yaml`), a Secret template `red-liner-auth` (key `BENCH_TOKEN`), and a Job `red-liner` that mounts the ConfigMap at `/etc/red-liner` and runs the image.

- [ ] **Step 1: Write `deploy/k3s/configmap.yaml`**

A `ConfigMap` named `red-liner-scenario` with one key `scenario.yaml`. The embedded scenario is self-contained (no `harvest`, no `query_file`/file pools) so a single mounted file suffices. Pin these values:

```yaml
apiVersion: v1
kind: ConfigMap
metadata:
  name: red-liner-scenario
data:
  scenario.yaml: |
    version: 1
    target:
      url: http://echo.default.svc.cluster.local      # in-cluster target Service
      protocol: rest
      timeout: 5s
      # auth:                       # uncomment and set the Secret in job.yaml to enable
      #   type: bearer
      #   token_env: BENCH_TOKEN
    load:
      profile: constant
      rate: 20
      duration: 15s
      warmup: 2s
    scenarios:
      - name: Root
        weight: 1
        request:
          method: GET
          path: /
    checks:
      status: [200]
    slos:
      success_rate: 0.99
      p95: 250ms
    report:
      dir: reports
      junit: true
```

- [ ] **Step 2: Write `deploy/k3s/secret.example.yaml`**

A template `Secret` named `red-liner-auth` with `stringData: { BENCH_TOKEN: "" }` and a header comment: copy to `secret.yaml`, fill in the token, `kubectl apply`. (The real `secret.yaml` is git-ignored in Task 3.)

- [ ] **Step 3: Write `deploy/k3s/job.yaml`**

A `Job` named `red-liner`:
- `spec.backoffLimit: 0`; `spec.template.spec.restartPolicy: Never`.
- container `red-liner`: `image: red-liner:dev`, `imagePullPolicy: Never`, `args: ["--config", "/etc/red-liner/scenario.yaml", "--out", "/tmp/reports"]`.
- volumes: a `configMap` volume (`red-liner-scenario`) mounted read-only at `/etc/red-liner`; an `emptyDir` mounted at `/tmp`.
- `securityContext` (container): `runAsNonRoot: true`, `readOnlyRootFilesystem: true`, `allowPrivilegeEscalation: false`, `capabilities: { drop: ["ALL"] }`, `seccompProfile: { type: RuntimeDefault }`.
- `resources`: requests `cpu: 100m, memory: 64Mi`; limits `cpu: 500m, memory: 256Mi`.
- a commented-out `env` entry wiring `BENCH_TOKEN` via `valueFrom.secretKeyRef` (name `red-liner-auth`, key `BENCH_TOKEN`), to enable auth without editing structure.

- [ ] **Step 4: Validate all three manifests client-side**

Run: `kubectl apply --dry-run=client -f deploy/k3s/configmap.yaml -f deploy/k3s/secret.example.yaml -f deploy/k3s/job.yaml`
Expected: each object validates (`configmap/red-liner-scenario`, `secret/red-liner-auth`, `job.batch/red-liner` all "created (dry run)"), no schema errors.

- [ ] **Step 5: Verify the embedded scenario parses with the real binary**

Extract the ConfigMap's `scenario.yaml` to a temp dir and validate it with the image (distroless has no shell, so validate via the binary, not inside the container):

```bash
mkdir -p /tmp/rl-scenario
kubectl create configmap x --dry-run=client -o jsonpath='{.data.scenario\.yaml}' \
  --from-file=scenario.yaml=<(yq '.data."scenario.yaml"' deploy/k3s/configmap.yaml) \
  > /tmp/rl-scenario/scenario.yaml 2>/dev/null \
  || yq '.data."scenario.yaml"' deploy/k3s/configmap.yaml > /tmp/rl-scenario/scenario.yaml
docker run --rm -v /tmp/rl-scenario:/etc/red-liner:ro --read-only --tmpfs /tmp \
  red-liner:dev --config /etc/red-liner/scenario.yaml --validate
```

Expected: exits 0 (the ConfigMap's scenario is a valid config the image accepts). If `yq` is unavailable, hand-copy the `scenario.yaml` block into `/tmp/rl-scenario/scenario.yaml` and run the `docker run` line.

- [ ] **Step 6: Commit**

```bash
git add deploy/k3s/configmap.yaml deploy/k3s/secret.example.yaml deploy/k3s/job.yaml
git commit -m "feat: K3s Job manifests for red-liner (configmap, secret template, hardened job)"
```

---

### Task 3: Developer workflow — Make targets, ignores, docs

**Files:**
- Modify: `Makefile`
- Modify: `.gitignore`
- Create: `deploy/k3s/README.md`
- Modify: `README.md`

**Interfaces:**
- Consumes: Task 1's image build and Task 2's manifests.
- Produces: `make docker-build image-save k3s-import k3s-deploy k3s-logs` targets wrapping the full local K3s flow.

- [ ] **Step 1: Add `.gitignore` entries**

Append: `red-liner-dev.tar` and `deploy/k3s/secret.yaml` (the filled-in secret must never be committed).

- [ ] **Step 2: Add Makefile targets**

Add to `.PHONY` and define:
- `docker-build:` → `docker build -t red-liner:dev .`
- `image-save:` → `docker save red-liner:dev -o red-liner-dev.tar`
- `k3s-import:` → `sudo k3s ctr images import red-liner-dev.tar`
- `k3s-deploy:` → `kubectl apply -f deploy/k3s/configmap.yaml -f deploy/k3s/job.yaml`
- `k3s-logs:` → `kubectl logs -f job/red-liner`

Keep the existing `##`-comment style used by the other targets.

- [ ] **Step 3: Verify a new target runs**

Run: `make docker-build`
Expected: builds `red-liner:dev` via the new target (same result as Task 1 Step 3).

- [ ] **Step 4: Write `deploy/k3s/README.md`**

A runbook covering: prerequisites (Docker + running K3s); the flow `make docker-build && make image-save && make k3s-import && make k3s-deploy`; reading the result via `kubectl wait --for=condition=complete job/red-liner --timeout=120s` (or `--for=condition=failed`) and `make k3s-logs`; a one-liner to stand up an in-cluster echo target to point the scenario at (e.g. `kubectl create deployment echo --image=ealen/echo-server --port=80 && kubectl expose deployment echo --port=80`); how to enable auth (copy `secret.example.yaml` → `secret.yaml`, set the token, `kubectl apply`, uncomment the `env` block in `job.yaml` and the `auth:` block in the scenario); exit-code meaning (0 pass / 1 SLO violation / 2 config error); the `ErrImageNeverPull` → re-run `k3s-import` note; and the documented extension points (PVC for reports, registry push, musl/scratch smaller image).

- [ ] **Step 5: Add a "Run in K3s" pointer to the top-level `README.md`**

A short subsection linking to `deploy/k3s/README.md`, placed near the existing "Install"/"Development" sections.

- [ ] **Step 6: Commit**

```bash
git add Makefile .gitignore deploy/k3s/README.md README.md
git commit -m "docs: K3s build/deploy make targets and runbook"
```

---

## End-to-end check (optional, needs a running K3s)

After all tasks, on a machine with K3s: `make docker-build image-save k3s-import`, stand up the echo target, `make k3s-deploy`, then `kubectl wait --for=condition=complete job/red-liner --timeout=120s && make k3s-logs`. Expected: the pod prints the `==== red-liner summary ====` block and the Job completes (exit 0) against the echo service.
