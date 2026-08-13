.DEFAULT_GOAL := help

.PHONY: help build test check fmt lint msrv deny ci install release

help:
	@printf '%s\n' \
		'Usage: make <target>' \
		'' \
		'Development:' \
		'  build    Build the debug binary.' \
		'  test     Run all feature-enabled tests.' \
		'  check    Type-check all targets and features.' \
		'  fmt      Check Rust formatting.' \
		'  lint     Run Clippy with warnings denied.' \
		'' \
		'CI:' \
		'  msrv     Build all features with the configured MSRV toolchain.' \
		'  deny     Check dependency licenses, bans, advisories, and sources.' \
		'  ci       Run formatting, lint, tests, MSRV build, and dependency checks.' \
		'' \
		'Release:' \
		'  install  Install the crate from this checkout.' \
		'  release  Build the release binary.'

build:
	cargo build --locked

test:
	cargo test --all-features --locked

check:
	cargo check --all-targets --all-features --locked

fmt:
	cargo fmt --check

lint:
	cargo clippy --all-targets --all-features --locked -- -D warnings

msrv:
	cargo build --all-features --locked

deny:
	cargo deny check licenses bans advisories sources

ci: fmt lint test msrv deny

install:
	cargo install --path . --locked

release:
	cargo build --release --locked
