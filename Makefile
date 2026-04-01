.PHONY: build release test lint fmt check ci deploy clean

build:
	cargo build

release:
	cargo build --release

test:
	cargo test --workspace

lint:
	cargo clippy --workspace -- -D warnings

fmt:
	cargo fmt --all

check:
	cargo fmt --all -- --check
	cargo clippy --workspace -- -D warnings
	cargo test --workspace

ci: check

deploy: release
	cargo install --path crates/cli

clean:
	cargo clean
