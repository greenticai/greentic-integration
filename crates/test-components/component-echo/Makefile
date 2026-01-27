.PHONY: build test lint check flows wasm

BUILD_FLAGS ?=

default: build

build:
	greentic-dev component build --manifest ./component.manifest.json $(BUILD_FLAGS)

flows:
	greentic-component flow update

wasm:
	cargo build --target wasm32-wasip2 --release

check:
	greentic-dev component doctor target/wasm32-wasip2/release/component_echo.wasm --manifest ./component.manifest.json

lint:
	cargo fmt --all
	cargo clippy --workspace --all-targets -- -D warnings

test:
	cargo test --workspace --all-targets
