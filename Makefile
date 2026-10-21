# Redliner — common developer tasks.
# CONFIG overrides the scenario file used by `run` / `validate`.
CONFIG ?= configs/graphql-catalog.yaml

.PHONY: build test lint install run validate clean \
        docker-build image-save k3s-import k3s-deploy k3s-logs

## build the release binary
build:
	cargo build --workspace --release

## run the full test suite
test:
	cargo test --workspace

## lint with clippy, warnings-as-errors
lint:
	cargo clippy --workspace --all-targets -- -D warnings

## install the `redliner` binary into ~/.cargo/bin
install:
	cargo install --path crates/redliner --locked

## run a scenario (make run CONFIG=configs/rest-example.yaml)
run:
	cargo run -p redliner -- --config $(CONFIG)

## validate a scenario config and exit
validate:
	cargo run -p redliner -- --config $(CONFIG) --validate

## remove build artifacts
clean:
	cargo clean

## build the container image (redliner:dev)
docker-build:
	docker build -t redliner:dev .

## save the image to a tar for offline import
image-save:
	docker save redliner:dev -o redliner-dev.tar

## import the saved image into K3s containerd (needs sudo)
k3s-import:
	sudo k3s ctr images import redliner-dev.tar

## apply the scenario ConfigMap and the Job to the cluster
k3s-deploy:
	kubectl apply -f deploy/k3s/configmap.yaml -f deploy/k3s/job.yaml

## follow the Job's logs
k3s-logs:
	kubectl logs -f job/redliner
