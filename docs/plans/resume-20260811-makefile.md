# Makefile implementation plan

## Objective

Add a root `Makefile` that provides a discoverable, thin interface to the repository's existing Cargo development, CI, and local release-build commands.

## Inputs

- [`docs/superpowers/specs/2026-08-11-makefile-design.md`](../superpowers/specs/2026-08-11-makefile-design.md): approved target contract.
- [`CONTRIBUTING.md`](../../CONTRIBUTING.md): canonical validation commands and flags.
- [`Cargo.toml`](../../Cargo.toml): crate features and release profile.

## Work

### 1. Add root Makefile

Create `Makefile` with `help` as the first/default target. Declare all public targets `.PHONY`.

Implement these direct recipes:

- `build`: `cargo build --locked`
- `test`: `cargo test --all-features --locked`
- `check`: `cargo check --all-targets --all-features --locked`
- `fmt`: `cargo fmt --check`
- `lint`: `cargo clippy --all-targets --all-features --locked -- -D warnings`
- `msrv`: `cargo build --all-features --locked`
- `deny`: `cargo deny check licenses bans advisories sources`
- `install`: `cargo install --path . --locked`
- `release`: `cargo build --release --locked`

Use Make prerequisites for `ci`, in exact order: `fmt lint test msrv deny`. Do not add shell conditionals, variables, toolchain switching, version logic, publication, Git operations, or configuration knobs.

Implement `help` with literal, stable output listing every public target and a concise description. It must not parse comments or depend on non-portable shell tooling.

### 2. Verify command interface

Run `make help`. Confirm all ten public targets are listed once: `build`, `test`, `check`, `fmt`, `lint`, `msrv`, `deny`, `ci`, `install`, and `release`.

Run `make -n` for each target. Confirm every printed command matches the approved specification and `ci` preserves prerequisite order.

### 3. Verify behavior

Run `make ci`. It must execute the documented checks in order and stop on the first failure. This requires the repository's configured Rust toolchain and locally installed `cargo-deny`.

## Non-goals

- Modifying CI workflows, Cargo configuration, documentation command examples, or release automation.
- Wrapping Cargo errors, installing missing tools, or selecting Rust toolchains.
- Automated packaging, tagging, publication, commits, and pushes.
