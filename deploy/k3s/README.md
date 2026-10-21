# Running Red Liner on K3s

Run Red Liner as a one-shot Kubernetes **Job** that fires a scenario at a
target service and gates on SLOs via its exit code. Reports are not persisted —
the result is the exit code plus the summary in the pod logs.

## Prerequisites

- Docker (to build the image).
- A running K3s (single node is fine).
- `kubectl` pointed at that cluster.

## Build and load the image

The image is loaded straight into K3s's containerd — no registry needed:

```bash
make docker-build     # docker build -t redliner:dev .
make image-save       # docker save redliner:dev -o redliner-dev.tar
make k3s-import       # sudo k3s ctr images import redliner-dev.tar
```

`job.yaml` sets `imagePullPolicy: Never`, so the pod uses exactly this imported
image. If you see `ErrImageNeverPull`, the import step was skipped or the tag
differs — re-run `make k3s-import`.

## Pick a target

The example scenario in [`configmap.yaml`](configmap.yaml) points at
`http://echo.default.svc.cluster.local`. Stand up a trivial in-cluster target
to try it against:

```bash
kubectl create deployment echo --image=ealen/echo-server --port=80
kubectl expose deployment echo --port=80
```

Edit `target.url` in `configmap.yaml` to hit your own service (any in-cluster
Service DNS name, or an external URL the pod can reach).

## Deploy and read the result

```bash
make k3s-deploy       # kubectl apply -f configmap.yaml -f job.yaml
kubectl wait --for=condition=complete job/redliner --timeout=120s \
  || kubectl wait --for=condition=failed job/redliner --timeout=5s
make k3s-logs         # kubectl logs -f job/redliner
```

The pod prints the `==== redliner summary ====` block (requests, success rate,
p50/p95/p99, per-operation breakdown, and SLO verdicts). The **exit code** is
the gate:

| exit | meaning |
| ---- | ------- |
| `0`  | run passed all SLOs |
| `1`  | an SLO was violated |
| `2`  | config or runtime error |

Re-run a Job after changes with:

```bash
kubectl delete job redliner --ignore-not-found
make k3s-deploy
```

## Authenticated targets

Credentials never live in the image or the ConfigMap. Use the Secret:

```bash
cp deploy/k3s/secret.example.yaml deploy/k3s/secret.yaml   # git-ignored
# edit secret.yaml, set BENCH_TOKEN
kubectl apply -f deploy/k3s/secret.yaml
```

Then uncomment the `env:` block in [`job.yaml`](job.yaml) and the `auth:` block
in the scenario in `configmap.yaml` (`type: bearer`, `token_env: BENCH_TOKEN`).

TLS targets work out of the box: the binary uses rustls with bundled Mozilla
roots, so no system CA configuration is needed in the container.

## Extension points (not built here)

- **Persist reports:** mount a PVC at the report dir instead of the `emptyDir`
  at `/tmp`, and point `--out` at it.
- **Registry:** push `redliner:dev` to a registry and set a normal
  `imagePullPolicy` for multi-node clusters.
- **Scheduled runs:** wrap the Job spec in a `CronJob`.
- **Smaller image:** a static musl build onto `scratch` (~15 MB); deferred
  because reqwest's rustls stack pulls `aws-lc-rs`, which needs a musl C
  toolchain.
