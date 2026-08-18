# Target
Plan the smallest disposable GitHub Actions workflow for the frozen CI-failure Issue experiment. Read-only investigation; do not edit application/workflow files, run formatters, linters, or project-wide suites.

# Inputs

- `.dispatch/LEDGER.md`
- `docs/superpowers/specs/2026-08-18-automated-issue-repair-design.md`
- `docs/plans/resume-20260818-automated-issue-repair.md`
- `.github/workflows/ci.yml` and current GitHub issue forms

# Output

Write `MINIMAL_WORKFLOW_MAP.md` in this worktree. Specify exact event filters, permissions, only required GitHub API calls/fields, deterministic fingerprint/title/body, dedupe behavior, labels, and manual dispatch test. Favor inline shell and GitHub CLI over new source or frameworks.

# Constraints

No checkout, artifacts, raw log access, agent repair, PR creation, secrets, `pull_request_target`, or durable ledger. The sole state is Issues and labels. Explain how deleting one workflow and labels removes the experiment.

# Exclusive files

`MINIMAL_WORKFLOW_MAP.md`, `.done/workflow-a1`.

# Completion

Commit the report. Write `.done/workflow-a1` containing the commit SHA.

# Verification

Cite current repo workflow paths and GitHub workflow event semantics. No formatters, linters, or project-wide suites mid-flight.
