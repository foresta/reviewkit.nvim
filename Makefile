.PHONY: build test test-rust check fmt

build:
	cargo build --release

test: test-rust

test-rust:
	cargo test --workspace

check:
	cargo fmt --all --check
	cargo clippy --workspace --all-targets -- -D warnings

fmt:
	cargo fmt --all
