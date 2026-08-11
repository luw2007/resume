# Changelog

All notable changes to this project will be documented in this file.

## Unreleased

## 0.2.1 - 2026-08-11

### Added

- Added a tag-triggered (`v*`) GitHub Actions release pipeline: builds the four target artifacts, publishes a GitHub Release with `SHA256SUMS` and build provenance attestations, and updates `luw2007/homebrew-tap` `Formula/resume.rb` via a scoped fine-grained personal access token (`HOMEBREW_TAP_PAT`).
- Added `--all-worktrees` to widen the default Git Scope to every linked worktree of the current repository; conflicts with `-U/--up`/`-D/--down`.

### Changed

- Default Git Scope now includes only the current worktree instead of every linked worktree, cutting a `git rev-parse` subprocess spawn per distinct recorded Workspace outside it (measured: ~99% of distinct Workspaces in a real multi-project OMP corpus) and removing the `git worktree list` spawn entirely from the default path. Use `--all-worktrees` to restore the previous default behavior.

### Fixed

- Full-binary QA re-verification pass (docs/qa/feature-inventory.csv, 185 user stories tested against `target/release/resume`) found and fixed six defects: `--since` filtered by raw transcript mtime instead of each integration's native-activity-first `updated_at`, causing false inclusions/exclusions; a too-small terminal or missing controlling terminal exited 1 instead of the documented usage exit 2 (the latter now also suggests `--list`/`--json`); `OMP_PROFILE` selecting a named profile silently dropped the default profile from "all profiles" discovery; Codex import metadata was parsed but never surfaced as a badge; a native Session title (e.g. Pi's `session_info.name`) could carry raw ANSI/OSC/bidi control bytes into `--list` and `--json` output; and the JSONL reader buffered an oversized line's full length before checking the size bound, so the advertised 8 MiB allocation cap didn't actually bound allocation. Corrected man page and product-design documentation that had drifted from a prior column-layout fix.

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
