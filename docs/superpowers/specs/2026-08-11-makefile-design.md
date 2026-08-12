# Makefile design

## Goal

Provide a small, discoverable Makefile for the repository's existing development, CI, and local release-build commands. It is a convenience interface only: Cargo and `cargo-deny` remain the command owners.

## Targets

The default `help` target lists these phony targets and their purpose:

| Group | Target | Command or composition |
| --- | --- | --- |
| Development | `build` | `cargo build --locked` |
| Development | `test` | `cargo test --all-features --locked` |
| Development | `check` | `cargo check --all-targets --all-features --locked` |
| Development | `fmt` | `cargo fmt --check` |
| Development | `lint` | `cargo clippy --all-targets --all-features --locked -- -D warnings` |
| CI | `msrv` | `cargo build --all-features --locked` |
| CI | `deny` | `cargo deny check licenses bans advisories sources` |
| CI | `ci` | Runs `fmt`, `lint`, `test`, `msrv`, then `deny` in that order. |
| Release | `install` | `cargo install --path . --locked` |
| Release | `release` | `cargo build --release --locked` |

## Constraints

- Declare every user-facing target `.PHONY`.
- Keep `help` as the default target and print only the stable target names and concise descriptions.
- Preserve the commands and flags documented in `CONTRIBUTING.md`; do not introduce a second validation standard.
- Do not add environment configuration, toolchain selection, version extraction, automatic tagging, publication, commits, or pushes.
- `deny` intentionally requires a locally installed `cargo-deny`, matching the contribution guide.

## Error handling

Each recipe exits with its invoked command's status. The aggregate `ci` target stops at the first failing prerequisite, as GNU Make normally does.

## Verification

- `make help` exposes all targets.
- Invoke each target's dry-run form (`make -n <target>`) and confirm its command and flags match this specification.
- Run `make ci` only when the required Rust toolchain and `cargo-deny` are available; it is the end-to-end CI-equivalent check.
