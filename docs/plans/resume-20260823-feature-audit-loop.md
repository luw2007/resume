# Feature Audit Loop — `resume`

## Context

`resume` is a ~25k-LOC Rust terminal launcher that discovers and resumes local coding-agent sessions (pi / claude / codex / omp / opencode). A canonical QA spreadsheet already exists at `docs/qa/feature-inventory.csv`: 189 features, each with `user_story` + `expected_behaviour` + `how_to_test`, all marked `Pass` from a 2026-08-11 real-binary pass.

That spreadsheet is now **60 commits stale**. Three entire feature areas landed since and have **zero** rows:

- **OpenCode integration** (`src/integration/opencode/`, `opencode` cargo feature, SQLite-only store)
- **Setup / first-run flow** (`src/settings.rs` — `run_setup`, `load_or_setup`, `parse_selection`, `newly_supported`, `refresh_known_agents`)
- **cmux workspace handoff** (`src/launch.rs:450` `handoff_cmux_workspace` + 18 error variants + fail-closed exec ordering)

Partially-stale areas: picker tab/page navigation (Tab/Shift-Tab/Left/Right/Alt-P/Alt-N, "show full newest page"), Codex parallel scan + persistent discovery cache + orphan pruning + background discovery. Additionally 132 of the 189 rows already carry `Citation update (...)` notes — the code references drifted even before these 60 commits.

Goal: restore the spreadsheet to a true canonical inventory, then run a full test → fix → retest loop against the real binary, so that every documented feature has a verified status.

Baseline established before planning: `cargo test --all-features` passes (exit 0). `src/app.rs` has one uncommitted test refactor (`title_column_width_for_tty`) that is complete and passing.

**Decisions confirmed with user:** fix scope = docs/UX **plus** real functional bugs (no architectural redesign); picker tested by **real PTY-driven interaction**; work lands on a **new branch with per-phase commits**.

## Authoritative sources for "every single feature"

1. `docs/product-design.md` (705 lines, §1–§8) — the product spec, section-numbered and already cited by existing rows and by test doc-comments.
2. The CLI surface — `src/cli.rs` (`Cli`, `Command`, `ConfigCommand`, `Distance`, `Since`, `Shell`, `validate`).
3. The code itself — `src/app.rs`, `picker.rs`, `launch.rs`, `settings.rs`, `scope.rs`, `session.rs`, `config.rs`, `diagnostics.rs`, `errors.rs`, `preview/`, `integration/{pi,claude,codex,omp,opencode}/`.
4. `README.md` §Install…§Shell completions — the user-facing promises.

A feature is in scope if it is user-observable. Repo infrastructure (CI workflows, issue templates, Makefile targets, benches) is **out of scope** — it is not app behavior.

---

## Phase 0 — Branch and clean baseline

1. `git switch -c qa/feature-audit`.
2. Commit the pending `src/app.rs` change on its own (`Test title column width across terminal sizes directly`) so the audit starts from a clean tree.
3. Copy this plan to `docs/plans/resume-20260823-feature-audit-loop.md` per global rule §10.
4. `cargo build --locked` — the audit drives `target/debug/resume`, the real binary.

## Phase 1 — Rebuild the canonical spreadsheet

Single canonical artifact stays at **`docs/qa/feature-inventory.csv`**. No second tracker.

**Column schema** (extends the existing 7 columns; keeps every current column name so prior content survives):

| column | meaning |
|---|---|
| `feature_id` | stable kebab-case key, area-prefixed (`cli-`, `picker-`, `opencode-`, `cmux-`, `setup-`, …) |
| `area` | *new* — grouping for reporting (`cli`, `picker`, `scope`, `launch`, `config`, `output`, `safety`, `diag`, `pi`, `claude`, `codex`, `omp`, `opencode`, `cmux`, `setup`) |
| `feature_name` | short label |
| `user_story` | "As a user, I want … so that …" |
| `expected_behaviour` | observable behavior, citing spec section and code path |
| `how_to_test` | concrete, runnable steps against the real binary |
| `spec_ref` | *new* — `docs/product-design.md` section, or `-` if code-only |
| `code_ref` | *new* — `file:line` (split out of `expected_behaviour` so drift is fixable in one column, not buried in prose) |
| `status` | *reset to* `Untested` in this phase; Phase 2 writes `Pass`/`Fail`/`Blocked` |
| `error_notes` | Phase 2 findings — symptom, repro, expected vs actual |
| `fix_ref` | *new* — Phase 3 commit / file:line of the fix, or `N/A` |
| `retest_status` | *new* — Phase 4 result: `Pass`/`Fail`/`N/A` |

Work:

1. **Re-derive the inventory** by walking `docs/product-design.md` §1–§8 section by section, cross-checking each claim against the code, and walking every `pub` entry point in `src/cli.rs` and the five integration modules.
2. **Refresh the 189 existing rows**: verify each `code_ref` still resolves, move citations from `expected_behaviour` prose into `code_ref`, and fold the 132 accumulated `Citation update (...)` notes into corrected refs. Drop a row only if the feature no longer exists — and record that removal in the commit message.
3. **Add rows for the uncovered areas** (61 rows added, 250 total):
   - `opencode-*` — SQLite-only discovery, `opencode` feature gate, the documented degradation to a single diagnostic with zero Sessions when the feature is compiled out (`discover_stub.rs`), resume path, roots, diagnostics.
   - `setup-*` — first-run selection prompt, `parse_selection` input grammar and its errors, `newly_supported` notification for agents added after a config exists, `refresh_known_agents` preserving retired/unknown fields, atomic `save` via temp + rename, `load_or_require_setup` vs `load_or_setup`.
   - `cmux-*` — handoff runs **before** native resume exec, fail-closed on every one of the 18 `CmuxHandoffError` variants, no-op when cmux env is absent, caller/workspace identity checks, workspace read-back verification, diagnostics preserved on failure.
   - `picker-*` additions — Tab/Shift-Tab/Left/Right/Alt-Left/Alt-Right tab switching, Alt-P/Alt-N paging, full-newest-page rendering, the vendored `skim-tuikit` Alt-arrow parser patch, `ctrl-r:ignore`, `Ctrl+O` preview toggle.
   - `codex-*` additions — background (never-blocking) discovery, parallel bounded scan, persistent discovery cache, orphaned-cache-entry pruning, numeric-timestamp SQLite enrichment.
   - `omp-*` addition — home-relative directory prefilter for hidden-dot workspaces (`c74b114`).
4. Commit: `Rebuild canonical feature inventory with coverage for opencode, setup, and cmux`.

**Gate:** every row has a non-empty `user_story`, `expected_behaviour`, `how_to_test`, and a `code_ref` that resolves in the current tree.

## Phase 2 — Test every user story, document every error

Two harnesses, both driving the real `target/debug/resume`. Neither invents a new isolation scheme — both reuse the env-isolation contract already encoded in `docs/qa/fixtures.sh` (isolated `HOME`, `XDG_*`, `PI_CODING_AGENT_DIR`, `PI_CONFIG_DIR`, `CLAUDE_CONFIG_DIR`, `CODEX_HOME`, `RESUME_DISABLE_PROC_PROBE=1`).

**A. Non-interactive harness** — extends `docs/qa/fixtures.sh`, which already builds one pi/claude/codex/omp session fixture matching `tests/step9_app.rs::fixtures()`. Additions needed: an OpenCode SQLite fixture, a `settings.json`-absent case for the setup flow, and a fake `cmux` on `PATH` (the pattern already exists in `src/launch.rs` tests around line 899). Drives every `--list` / `--json` / `--man` / `--verbose` / `--since` / `-U` / `-D` / `--all-worktrees` / `--agent` / `--config` / `config example` / `completions` case, capturing stdout, stderr, and exit code per row.

**B. Interactive PTY harness** — new `tests/picker_ux_e2e.rs`, modeled directly on the existing `tests/picker_spike.rs` session harness (`portable_pty`, background reader thread, `PtySize`, the `SPIKE_PTY_TESTS=1`-style skip guard when no PTY is available) and on `tests/cmux_handoff_e2e.rs`. The key difference: `picker_spike.rs` drives the `resume-spike` **example**, whereas this drives the real `resume` binary against the fixture HOME. Covers: picker opens and streams rows; `Ctrl+O` preview toggle; `Tab`/`Shift-Tab`/`Left`/`Right` tab switching; `Alt-P`/`Alt-N` paging; newest page renders in full; row ordering is newest-first and not fuzzy-reordered; search narrows; `Enter` reaches the confirmation path; `Ctrl+C` exit code.

This harness is the mechanism for the requested testing and it makes Phase 4 a rerun rather than a repeat of manual work.

Execution: run every row's `how_to_test`, write `Pass` / `Fail` / `Blocked` into `status`, and for each `Fail` record symptom + exact repro + expected vs actual in `error_notes`. Report per-area counts, not per-row narration.

Commit: `Record feature-inventory test results` (+ the harness commits).

## Phase 3 — Fix every logistical and UX error

Triage the `Fail` rows into two buckets and fix both:

- **Logistical** — inventory rows whose `expected_behaviour` or `code_ref` misdescribes the code, stale spec text in `docs/product-design.md` / `README.md`, wrong or misleading `--help` / `--man` text, inconsistent error message wording, exit codes that disagree with `src/errors.rs`.
- **UX** — confusing or unactionable error messages, missing suggestions on failure paths, row/column layout defects, ordering surprises, keybindings that are documented but not bound (or bound but undocumented).
- **Functional bugs** surfaced by testing are in scope per the confirmed decision. Per global rule §7, each gets a **reproducing test that fails first**, then the fix. Architectural redesign and behavior-semantics changes that would contradict `docs/product-design.md` are out of scope — those get recorded in `error_notes` and raised, not silently implemented.

Each fix records its commit or `file:line` in `fix_ref`. Commit per logical unit, never `git add -A`.

**Boundary:** if triage exceeds ~25 defects or any fix requires changing a documented behavior contract in `docs/product-design.md`, stop and report before proceeding — that crosses from "fix errors" into "change the product".

## Phase 4 — Retest every user story

Rerun both harnesses in full — not just the previously-failing rows, since fixes can regress passing behavior. Fill `retest_status` for every row (`N/A` only for rows that were never `Fail` **and** whose area saw no fix). Any row still failing keeps its `error_notes` updated with the residual symptom and is reported explicitly rather than quietly left.

Commit: `Record post-fix feature-inventory retest results`.

## Verification

- `make ci` — `cargo fmt --check`, `cargo clippy --all-targets --all-features -D warnings`, `cargo test --all-features --locked`, MSRV build, `cargo deny`. Must pass at the end of Phase 3 and Phase 4.
- `cargo test --all-features` including the new `tests/picker_ux_e2e.rs`, plus `--features codex-sqlite` and `--features opencode` builds, since `opencode` discovery is inert without its feature.
- CSV integrity check: parses as CSV, no duplicate `feature_id`, no empty `status`/`retest_status`, every `code_ref` resolves.
- Final report: per-area totals for Phase 2 status vs Phase 4 `retest_status`, the full defect list with dispositions, and anything explicitly left unfixed with the reason.

## Files

- `docs/qa/feature-inventory.csv` — rewritten and extended (the single canonical tracker)
- `docs/qa/fixtures.sh` — extended with opencode / setup / fake-cmux fixtures
- `tests/picker_ux_e2e.rs` — new PTY harness
- `docs/plans/resume-20260823-feature-audit-loop.md` — this plan
- Phase 3 fixes: `src/` and `docs/product-design.md` / `README.md`, scoped by triage
