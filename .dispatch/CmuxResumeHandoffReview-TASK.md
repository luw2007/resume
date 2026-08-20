# Target
Review the current uncommitted `resume` cmux workspace-handoff implementation for correctness, security, and adherence to the accepted reports. Read-only: do not edit source, docs, or task files; do not run formatters, linters, or project-wide suites.

# Inputs
- `CMUX_RESUME_HANDOFF_PLAN.md`
- `CMUX_RESUME_HANDOFF_CHANGE_DETAILS.md`
- Current diff and `src/launch.rs`, `src/app.rs`, `docs/product-design.md`

# Acceptance
Verify:
- Both absent cmux IDs performs no lookup or mutation.
- Partial/empty IDs fail before mutation.
- Verified-cmux path calls identify, pre-list, exactly one addressed `surface.report_pwd` with canonical target, then post-list before native `exec`.
- No focus/select/rollback/retry; `app_cli_path` is reused.
- The actual tests prove their named semantics rather than merely exhausting fixtures.
- Error paths remain fail-closed and diagnostics do not leak unbounded content.

# Output
Write `CMUX_RESUME_HANDOFF_REVIEW.md` with PASS or severity-ranked blockers. Commit only that report. Write `.done/cmux-handoff-review-a1` containing its commit SHA.

# Constraints
Do not request scope expansion. Findings must cite concrete paths/symbols and propose the smallest correction.