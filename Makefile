.PHONY: build release test lint fmt check ci deploy clean

build:
	cargo build

release:
	cargo build --release

test:
	cargo test

lint:
	cargo clippy -- -D warnings

fmt:
	cargo fmt

check:
	cargo fmt -- --check
	cargo clippy -- -D warnings
	cargo test

ci: check

deploy: release
	cargo install --path .

clean:
	cargo clean
