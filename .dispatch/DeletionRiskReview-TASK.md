# Target
Adversarially review the frozen disposable CI-failure Issue experiment. Read-only investigation; do not edit source/workflows, run formatters, linters, or project-wide suites.

# Inputs

- `.dispatch/LEDGER.md`
- `docs/superpowers/specs/2026-08-18-automated-issue-repair-design.md`
- `docs/plans/resume-20260818-automated-issue-repair.md`
- existing `.github/workflows/*.yml`, Issue forms, and `src/diagnostics.rs`

# Output

Write `DELETION_RISK_REVIEW.md` in this worktree. Find every way a proposed workflow could accidentally become durable, privileged, noisy, or privacy-breaking. For each finding, prescribe the smallest constraint or delete the feature. Conclude with a go/no-go checklist for a two-week report-only pilot.

# Constraints

Reject agent runners, repair PRs, source checkout, artifact/log ingestion, database/ledger, source modules, and workflow permissions beyond contents read/issues write. Keep only the proposed metadata-only notifier if defensible.

# Exclusive files

`DELETION_RISK_REVIEW.md`, `.done/risk-a1`.

# Completion

Commit the report. Write `.done/risk-a1` containing the commit SHA.

# Verification

Ground claims in current files and GitHub Actions trust/permissions mechanics. No formatters, linters, or project-wide suites mid-flight.
