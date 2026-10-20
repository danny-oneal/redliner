# Red Liner — common developer tasks.
# CONFIG overrides the scenario file used by `run` / `validate`.
CONFIG ?= configs/graphql-catalog.yaml

.PHONY: build test lint install run validate clean

## build the release binary
build:
	cargo build --workspace --release

## run the full test suite
test:
	cargo test --workspace

## lint with clippy, warnings-as-errors
lint:
	cargo clippy --workspace --all-targets -- -D warnings

## install the `red-liner` binary into ~/.cargo/bin
install:
	cargo install --path crates/red-liner --locked

## run a scenario (make run CONFIG=configs/rest-example.yaml)
run:
	cargo run -p red-liner -- --config $(CONFIG)

## validate a scenario config and exit
validate:
	cargo run -p red-liner -- --config $(CONFIG) --validate

## remove build artifacts
clean:
	cargo clean
