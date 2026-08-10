# Changelog

All notable changes to this project will be documented in this file.

## Unreleased

### Added

- Added a tag-triggered (`v*`) GitHub Actions release pipeline: builds the four target artifacts, publishes a GitHub Release with `SHA256SUMS` and build provenance attestations, and updates `luw2007/homebrew-tap` `Formula/resume.rb` via a scoped fine-grained personal access token (`HOMEBREW_TAP_PAT`).

## 0.2.0 - 2026-08-10

### Changed

- List and picker rows now show the native Session update time with a documented file-mtime fallback, human-relative dates, and the Git worktree branch as `+ <branch>`.
- Picker Preview identifies the full local update timestamp and whether it came from native metadata or the session file modification time.

## 0.1.0 - 2026-08-09

### Added

- Bootstrapped the standalone Rust 1.91+ CLI and established the core Session, identity, launch, diagnostics, and configuration model.
- Added the complete command-line surface and directory-derived Scope semantics for Git worktrees, explicit upward/downward Directory Distance, and exact non-Git fallback.
- Embedded Skim for streamed fuzzy selection and terminal-safe Preview, including opaque identity selection, `Ctrl-O` Preview toggle, and the safe dual-section fallback with `Ctrl-R` ignored.
- Added bounded, read-only JSONL/text/message/Preview foundations with terminal-control neutralization, redacted diagnostics, cancellation, caching limits, and partial/malformed input handling.
- Added independent Pi, Claude Code, Codex, and OMP integrations with fixture-tested discovery, presentation parsing, isolation provenance, and exact native Resume contracts.
- Added optional, feature-gated Codex `state_5.sqlite` read-only enrichment that degrades to authoritative JSONL discovery.
- Assembled production picker, list and JSON modes, risk confirmation, launch revalidation, terminal restoration, and shell-free process replacement.
- Added v1 JSON output documentation, support matrix, privacy/diagnostics guidance, and generated Bash, Zsh, and Fish completion scripts.

### Notes

- v0.1.0 is installed from Git; publishing to crates.io is not part of this release.
