# Redliner — common developer tasks.
# CONFIG overrides the scenario file used by `run` / `validate`.
CONFIG ?= configs/graphql-catalog.yaml

.PHONY: build test lint install test-install test-release release release-major release-minor release-patch \
        run validate clean \
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
	cargo install --path crates/redliner-import --locked

## test scripts/install.sh against a fake local release
test-install:
	sh scripts/test-install.sh

## test scripts/release.sh and scripts/changelog-section.sh
test-release:
	sh scripts/test-release.sh

## prepare a release: bump the version and date the changelog (make release VERSION=0.2.0)
release:
	sh scripts/release.sh $(VERSION)

## prepare a release that bumps one part of the current version: 0.3.0 -> 1.0.0 / 0.4.0 / 0.3.1
release-major release-minor release-patch:
	sh scripts/release.sh $(@:release-%=%)

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
