.PHONY: build test lint bench bench-criterion clean fmt

build:
	cargo build --release

test:
	cargo test

lint: fmt-check clippy

fmt:
	cargo fmt

fmt-check:
	cargo fmt --check

clippy:
	cargo clippy -- -D warnings

bench:
	cargo test --release --test llm_bench -- --nocapture --ignored

bench-criterion:
	cargo bench

clean:
	cargo clean
