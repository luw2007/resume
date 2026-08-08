# Adversarial code review — round 1

Reviewer: independent worker (task_ed1ead60b0f4). Scope: full repo against
`plans/v0.1.0-implementation.md` (all 13 steps, Non-negotiable invariants,
Plan mutation protocol), `CONTEXT.md`, and
`docs/adr/0001-rust-and-skim-for-terminal-session-picker.md`, focused on
`src/` and `tests/`. No code changes were made; this is review only.

## Verification performed

- `cargo test --locked --all-features` -> 330 unit tests + 5 property tests +
  12 PTY tests + 3 app tests, all pass (0 failed).
- `cargo clippy --all-targets --all-features --locked -- -D warnings` -> clean.
- `cargo fmt --check` -> clean.

All three gates are green at the reviewed commit
(`c1b0a80 merge: Step 11 product documentation and support claims`).

## 1. [BLOCKING] Symlinked Session transcripts are silently dropped (Pi/OMP), or read unconfined (Claude/Codex); `jsonl::read_file_confined` is dead code in production

Files:
- src/integration/pi.rs:422-441 (collect_jsonl)
- src/integration/omp/mod.rs:558-576 (collect_jsonl)
- src/integration/claude/mod.rs:213-221 (is_transcript), :252 (read_file call)
- src/integration/codex/mod.rs:337-359 (list_rollout_files_into), :439 (read_file call)
- src/jsonl.rs:94-112 (open_for_read), 206-220 (read_file vs read_file_confined)

Pi and OMP discovery filters with `file_type.is_file()`, which is false for
symlinks (lstat-based), so symlinked .jsonl files are silently excluded from
discovery entirely -- not read, not reported. This contradicts the pi.rs doc
comment claiming "does read symlinked files (confined at the API boundary)".
The existing "dedupe via symlink" tests (pi/tests.rs:679-701,
omp/tests.rs:940-947) only assert the session count didn't grow to 2, which
is equally true whether deduped or silently skipped -- confirmed empirically
that it's actually skipped, not deduped.

Claude Code and Codex intentionally follow symlinks
(`is_file() || is_symlink()`), and read them via `jsonl::read_file` which
calls `open_for_read(path, None)` -- no effective_root passed, so the
existing symlink-confinement check in open_for_read is never exercised.
`jsonl::read_file_confined` exists, is unit-tested, and is exactly what
Non-negotiable invariant #6 ("never follow a session-file symlink outside
the effective configured root") requires -- but it is called from zero
production call sites in any of the four integrations.

Concretely confirmed: a symlinked rollout file under CODEX_HOME pointing
outside the effective root is discovered, read, and surfaced as a resumable
Session with its foreign content intact. Same for Claude Code under
CLAUDE_CONFIG_DIR/projects.

Why it matters: this is a live gap against invariant #6 for two of the four
required-Supported integrations (Codex, Claude Code) -- a planted symlink
can leak arbitrary file content (e.g. another user's home directory) into
the picker as a "resumable session". For Pi/OMP it's currently masked by an
unrelated bug (symlinks aren't traversed at all) but reintroducing symlink
traversal there (the "obvious" fix matching the doc comment) would inherit
the same unconfined-read exposure unless read_file_confined is used.

Suggested fix: route all four integrations' `jsonl::read_file(path, bounds)`
call sites through `jsonl::read_file_confined(path, effective_root, bounds)`
using each integration's already-resolved effective root; add symlink
confinement regression tests at the integration level (not just jsonl.rs);
decide and fix Pi/OMP's symlink-file discovery to match either the doc
comment (discover + confine) or update the comment to match current
skip-of-symlinks behavior, and fix the "dedupe" tests to actually
distinguish dedupe from silent skip.

## 2. [NON-BLOCKING] `--verbose` diagnostic redaction module is dead code; actual verbose stderr path bypasses it

Files: src/diagnostics.rs (all), src/app.rs:700-711 (print_diagnostics)

src/diagnostics.rs implements redact_text/redact_path and a
DiagnosticCollector with good tests, but nothing outside the module calls
it. app.rs::print_diagnostics prints `Diagnostic.verbose_path`/
`verbose_chain` directly to stderr under --verbose without ever routing
through redact_text/redact_path. README's privacy claim ("--verbose ...
does not log message bodies or sensitive remotes/URLs") holds today only by
happenstance (current producers are io::Error/rusqlite::Error strings or
static text), not because anything enforces it. A future diagnostic
producer that echoes a bound SQL value or a path containing a URL-like
substring would silently violate the documented contract with no test to
catch it.

Suggested fix: wire redact_text/redact_path into
app.rs::print_diagnostics before printing verbose_path/verbose_chain, or
remove diagnostics.rs if session::Diagnostic producers are considered
sufficiently trusted by design.

## 3. [NITPICK] `RiskStatus::ConflictingMetadata` and `RiskStatus::WorkspaceChanged` are modeled but never produced by any discovery path

Files: src/session.rs:52-55, src/launch.rs:149-151, all four integration mod.rs files.

Only RiskStatus::Normal and RiskStatus::BroadWorkspace are ever assigned by
any integration. WorkspaceChanged risk is independently and correctly
caught at exec time by launch::revalidate regardless of this field, so this
is not a safety bug, just unreachable functionality / a documentation note
for a possible follow-up.

## What I did not find (verified clean)

- Shell-free exec: launch::exec uses CommandExt::exec with discrete argv, no shell string.
- Opaque Skim selection: SkimItem::output() returns only an opaque key; selection resolves via CandidateKey -> HashMap lookup, never a row index or display string.
- Revalidate-before-exec: app.rs::resume_selected calls launch::revalidate (transcript identity, CLI availability, Workspace) before any confirmation or exec; tested with real filesystem mutation between capture and revalidation.
- Positive-evidence-only Activity: all four integrations default to Unknown absent explicit matched evidence; app.rs never actually supplies evidence today, consistent with README's honest disclosure.
- Claude Code UUID/embedded-sessionId agreement contract implemented exactly as documented.
- JSONL parser bounds (line/file/record/nesting, malformed-middle isolation, incomplete-tail, concurrent-writer) genuinely enforced and tested; no panic/unbounded-allocation path found.
- Terminal-safety stripping (ANSI/CSI/OSC8/OSC52/C1/bidi) thorough, covered by adversarial + proptest tests; no bypass found.
- injection.rs <skill> collapsing is conservative and spec-compliant; arbitrary XML survives.
- Per-integration independence holds structurally; no cross-integration imports; shared code is schema-agnostic.
- README/docs support-matrix claims appropriately hedged, not overclaimed (Preview title precedence, since flag, Ctrl-R behavior all correctly qualified).
- Codex SQLite enrichment (Step 8) correctly gated behind default-off feature, read-only, schema-tolerant, rollout-authoritative with disagreement->diagnostic; delete-DB-no-change proven by test.

## Verdict

1 blocking issue (symlink confinement gap, invariant #6 violation for
Codex/Claude Code). 1 non-blocking issue (unused redaction module).
1 nitpick. All build/test/lint gates green. Recommend fixing #1 before v0.1.0.
