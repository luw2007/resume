# Changelog

All notable changes to this project will be documented in this file.

## Unreleased

## 0.3.1 - 2026-08-14

### Added

- Added `resume setup` and `~/.resume/settings.json` agent selection. First use chooses which integrations scan; `-a/--agent` and configured TOML `agents` still override it. New integrations are announced once after an upgrade but are never enabled without rerunning `resume setup`.

### Fixed

- Replaced maintainer-specific integration evidence and test paths with synthetic data.

### Changed

- Bundled the MIT license and provenance notice for the patched `skim-tuikit` 0.6.6 source.

## 0.3.0 - 2026-08-12

### Added

- The interactive Picker now waits only for the directory-scanning agents (Pi, OMP, Claude) to finish discovery (printing one progress line per agent to stderr as it completes) before opening; when Codex is configured alongside at least one other agent, it discovers in the background instead of holding the Picker closed, since its per-file JSONL parsing cost is not bounded the way the directory-pruned agents' scans are (observed on a real corpus: sub-200ms for Pi/OMP/Claude, 18+ seconds for Codex). Codex's Sessions merge into the shared candidate list as soon as its scan finishes and appear on the next tab switch or page turn -- no relaunch needed -- and its own progress line prints once the Picker has released the terminal. A `(codex still scanning)` header hint shows while it is still running. When Codex is the only configured agent it stays synchronous, since there is nothing else to show while waiting. The Picker presents an `All` tab plus one tab per agent with data so far; each tab is sorted oldest-first with the most recently updated Session last and paginated at 50 per page, opening on its newest page. `Alt+P`/`Alt+N` page within the current tab, `Alt+Left`/`Alt+Right` switch tabs (wrapping). Replaces the prior instant-open, unsorted live stream.
- Codex discovery now runs its own file scan across up to 8 bounded worker threads instead of one file at a time, and maintains a small, non-authoritative discovery cache at `$XDG_CACHE_HOME/resume/codex-discovery-v1.json` keyed by each rollout's (path, size, mtime): an unchanged file's parsed content is reused instead of re-read and re-parsed, and an entry for a rollout deleted since it was cached is pruned on the next run that scans the same `CODEX_HOME` (an entry from a *different* `CODEX_HOME` is never touched, since that run has no fresh evidence about it). Both the parallel scan and the cache are scoped exceptions to the general "one discovery worker per integration, sequential scan, no persistent cache" design, justified by Codex being the one integration whose scan time is not bounded by Scope (its rollout store has no Workspace-encoded directory names to prune by, unlike Pi/OMP/Claude). Measured on a real corpus (3546 rollouts, ~2.9 GB): a cold parallel scan of a 1016-session Scope took 3.95s (down from single-threaded's up to 18-19s observed on the same corpus); a warm-cache rerun took 0.17-0.19s (20-23x faster than cold), with byte-identical output confirmed against the uncached path. The cache is purely a speed optimization -- the rollout JSONL remains authoritative, a missing/corrupt/version-mismatched cache file silently degrades to a full fresh scan, and deleting it is always safe.
- Tab/Shift-Tab and the bare Left/Right arrow keys now also switch the picker's agent tabs, alongside the existing Alt-Left/Alt-Right, so terminals or habits that never reach the Alt-modified form still work. Trades away Skim's default arrow-key cursor movement inside the typed filter query.

### Changed

- Pi, OMP, and Claude discovery now prune whole grouped Workspace directories by their encoded directory name before reading any file: a dash-prefixed directory whose lossy-decoded name cannot correspond to any in-Scope Workspace is skipped entirely (`Scope::may_contain_session_dir`; both sides normalized to the coarsest encoding, every non-alphanumeric character -> `-`, covering Pi/OMP's `/`-only mapping and Claude's full non-alphanumeric collapse). The header `cwd` stays authoritative for every file that is read, and custom session roots (flat layouts) are never pruned. Measured against real corpora inside this repository's default Scope: OMP `--json` 4.85s -> 0.26s, Pi 1.72s -> 0.06s, Claude 0.92s -> 0.01s, with identical discovered session sets. Codex is unaffected: its store is date-partitioned (`sessions/YYYY/MM/DD/`), carries no Workspace encoding, and already uses a bounded 64 KiB early read.
- Codex discovery now applies an early per-file Workspace gate: with a Scope filter active, a first-record read (`max_records: 1`, stopping at the first parsed record regardless of the `session_meta` line's size) resolves `payload.cwd`, and an out-of-Scope rollout skips the 64 KiB title-derivation read entirely. Codex's date-partitioned store has no Workspace-encoded directories, so this is its equivalent of the Pi/OMP/Claude directory pruning. Measured on a real 3582-rollout corpus (interleaved min-of-4): `--json --agent codex` 2.64s -> 1.66s with byte-identical output.
- The tabbed picker's rows now preserve chronological (rank-ascending, newest-last) order instead of being reordered by Skim's default fuzzy-match sort against an empty query (`no_sort`/`tac`).

### Fixed

- Alt-Left/Alt-Right did not switch the picker's agent tabs on macOS terminals (Ghostty, iTerm2, Terminal.app): `skim-tuikit` 0.6.6's CSI parser recognized the xterm modifier form for Alt-Up/Down/Home/End (`CSI 1;3{A,B,H,F}`) but not Alt-Left/Alt-Right (`CSI 1;3{C,D}`), so those keys fell through to an unrecognized-sequence error and Skim never saw the tab-switch bind. Vendored `skim-tuikit` with the two missing arms added and patched it in via `[patch.crates-io]`.

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
