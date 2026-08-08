# Contributing

Thank you for contributing to `resume`.

## Before opening a change

- Search existing issues and pull requests to avoid duplicate work.
- For substantial behavior or interface changes, open an issue first so the approach can be agreed on.
- Keep changes focused, preserve the terminology in [`CONTEXT.md`](CONTEXT.md), and update user-facing documentation when behavior changes.
- Do not include real coding-agent session data, credentials, private paths, or other sensitive information in fixtures, logs, issues, or pull requests.

## Development setup

Install Rust 1.91 or newer and clone the repository. The lockfile is committed; use `--locked` for reproducible dependency resolution.

Run the same quality commands used by CI on the stable toolchain:

```sh
cargo fmt --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --all-features --locked
```

CI also builds with the Rust 1.91.0 minimum supported toolchain:

```sh
cargo build --all-features --locked
```

Finally, run the dependency policy check:

```sh
cargo deny check licenses bans advisories sources
```

The final command requires [`cargo-deny`](https://github.com/EmbarkStudios/cargo-deny). Install and select Rust 1.91.0 before the MSRV build (for example, with a directory override) so that command exercises the same toolchain as CI.

## Pull requests

Explain the problem and the chosen solution, link related issues, and describe how the change was verified. Add or update tests for behavior changes. By submitting a contribution, you agree that it is licensed under the repository's MIT License.
