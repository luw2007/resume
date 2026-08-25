# Changelog

All notable changes to this project will be documented in this file.

## Unreleased

## 0.3.6 - 2026-08-24

### Fixed

- CI: the cmux workspace handoff test's embedded fake-`cmux` shell script used bash's `$(<file)` fast-read extension, which `dash` (Ubuntu's default `/bin/sh`) does not support -- it silently expanded to nothing, so the `workspace` subcommand's reported `current_directory` came back empty and the production handoff's read-back check correctly rejected the mismatch, failing `cargo test` on `ubuntu-latest` CI (invisible locally on macOS, whose `/bin/sh` tolerates the bash extension). This predates 0.3.5's own work. Replaced with the POSIX `read -r current < "<path>"` builtin, which is portable across `dash`/`bash` and needs no external `cat` binary (the test deliberately narrows `PATH` to only its fake executables).

## 0.3.5 - 2026-08-24

### Fixed

- Pi's `resume_spec` canonicalized the transcript and `--session-dir` paths, resolving filesystem symlinks (e.g. macOS's `/var -> /private/var`) into the resume argv -- the same anti-pattern the 0.3.4 Codex workspace fix explicitly rejected, reproduced here for Pi. Paths are now made absolute without resolving symlinks, matching Codex/Claude/OMP's verbatim-path contract.
- OMP's imported-Session badge validated the full, un-truncated `origin_id` against its 32-byte safety cap before truncating to the displayed 8-character prefix, so any origin ID longer than 32 bytes (a 36-byte UUID, or any realistic long session ID) silently dropped the whole `origin:` fragment instead of showing the intended safe prefix. The safety check now runs on the already-truncated prefix.
- OMP discovery's home-relative directory prefilter compared an uncanonicalized `$HOME` against the canonical Scope base, so a symlinked `$HOME` (common on Linux hosts with home mounted elsewhere) could make every OMP Session invisible. `$HOME` is now canonicalized before the comparison.

### Added

- Added a real-binary QA regression suite (`docs/qa/run_checks.py`, `docs/qa/check_inventory.py`) that drives the compiled `resume` binary against isolated fixtures for the full `docs/qa/feature-inventory.csv` behavior inventory, plus a citation-integrity checker that verifies every inventory row's file:line references still point at the symbol they cite.

## 0.3.4 - 2026-08-24

### Fixed

- `--up`/`--down` conflicts were fully absorbed by clap's own `conflicts_with`, so `--man`'s documented four-line `ERROR [E1002] CONFLICTING_DIRECTION` block never actually rendered; the conflict is now checked and reported through resume's own error catalog.
- Session ordering, terminal safety, and diagnostics: `compare_sessions` had drifted to a timestamp-only comparator, silently dropping the documented Active-first/Inactive/Unknown priority (the picker's `sort_rank` now matches); Git branch labels are now sanitized through the terminal-safe text pipeline (a malicious branch name could otherwise spoof output via bidi control characters); the verbose diagnostics base64-redaction loop corrupted adjacent multibyte UTF-8 text; `errors::Report::fmt` no longer lets a multiline error detail break the documented four-line ERROR block; `--list`/`--json` no longer panic on a closed stdout pipe (e.g. `resume --list | head -1`); `preview::PreviewItem::render` now sanitizes content and the filesystem-derived path again at its own display boundary.
- Codex: workspace `cwd` was canonicalized (following filesystem symlinks such as macOS's `/var -> /private/var`), producing a different `workspace` value than Claude/Pi/OMP for the same logical directory; it now keeps the recorded path verbatim like the other integrations. `extract_import` (Codex and OMP) aborted badge extraction for an entire transcript on the first record missing a `payload` key instead of skipping just that record and continuing; both import badges now also sanitize their text through a safe-token filter. Codex's schema-detection heuristic could pick an unrelated table exposing only a generic `id`/rollout-path column; it now also requires an enrichment column.
- Pi: `resume_spec` could emit a relative transcript or `--session-dir` path in the resume argv under a relative discovery root, violating the exact-resume contract; both paths are now canonicalized before argv construction.
- Claude: a projects root that exists but can't be resolved (e.g. permission-denied) was silently treated as absent instead of surfacing `claude_root_unavailable`.
- OMP: named profiles with `PI_CODING_AGENT_DIR` set (which only applies to the default profile) incorrectly skipped their own `XDG_STATE_HOME` breadcrumb lookup; message attribution now also checks a nested `meta.source` envelope, not just the top-level injected/automated flags.
- `Scope::canonical_base` silently accepted a canonicalized path that isn't a directory instead of rejecting it as a usage error, contradicting `--man`'s documented contract.

### Changed

- Corrected several `--help`/`--man`/README/`docs/product-design.md` drift items found during a full 190-row feature-inventory QA re-audit: the default Git Scope is the current worktree only (`--all-worktrees` widens it, not the reverse); `config example`'s emitted `agents` is a conservative starter selection, not the actual runtime default (which is `Settings::default()`'s full agent set); `opencode` was missing from the documented valid-agent list; `XDG_CONFIG_HOME` is only used when the file exists, not unconditionally; a stale `opencode_disabled` diagnostic category was documented but never emitted (both a missing and a feature-off database already report `opencode_root_unavailable`); and the interactive picker opens once Pi/Claude/OMP discovery completes with Codex still scanning in the background, not after full discovery as README previously said.

## 0.3.3 - 2026-08-23

### Added

- Added a cmux workspace handoff: when `resume` is launched from inside a cmux-managed workspace/surface pair (`CMUX_WORKSPACE_ID` and `CMUX_SURFACE_ID` both present) and the selected Session's directory differs from the caller's, `resume` now reports the canonical target directory to cmux (`surface.report_pwd`) before replacing itself with the resumed agent, so cmux's own workspace state tracks the directory the agent will actually run in. Verified against both IDs present, absent, and partial/malformed provenance (fails closed, no handoff attempted, no focus/selection change).
- Added a `CI failure issue` GitHub Actions workflow that opens or updates a single metadata-only triage issue when the `CI` workflow fails on `main`, instead of leaving failures to be found by hand.

### Fixed

- Pi, OMP, and Claude discovery's home-relative directory prefilter silently dropped every Session under a workspace whose first path component after `$HOME` starts with `.` (e.g. `~/.omp/agent`): the lossy path-to-directory-name encoding collapses both the path separator and the leading `.` to `-`, producing a doubled separator that `candidate_keys` only stripped one character of, so the derived key never matched the on-disk directory name and `Scope::may_contain_session_dir` pruned the directory before reading any file inside it. Sessions under a hidden-dot leading component now match correctly.
- The tabbed picker could open on a partial page instead of the newest full page of `PAGE_SIZE` Sessions when an older, shorter remainder page existed: paging is now newest-first from a full page, and the header states the exact count of older Sessions still available via `Alt-P`.

## 0.3.2 - 2026-08-16

### Fixed

- Codex `state_5.sqlite` enrichment degraded to `codex_sqlite_degraded: query_failed` on every discovery run against a modern Codex install, because `threads.updated_at` is stored as an `INTEGER` Unix timestamp while the query decoded it as `Option<String>`, and any type-mismatch aborted the whole enrichment query. Activity-time decoding now accepts ISO-8601 text, integer epoch seconds, integer epoch milliseconds, and floating-point seconds, so native Codex activity timestamps enrich normally again.

### Changed

- Cleared the Clippy warnings `make ci`'s lint gate was not yet enforcing (a manual loop counter, a redundant single-element slice clone, unneeded borrows, a collapsible nested `if let`, and a function with too many arguments) across `examples/resume-spike.rs`, `src/integration/codex/cache.rs`, `src/integration/pi/tests/discover.rs`, and `src/picker.rs`. No behavior change.

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
