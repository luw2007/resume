# Disposable CI-failure Issue experiment plan

## Outcome

Add one deletion-friendly GitHub workflow that creates or updates a triage Issue for a failed trusted `main` CI run. It must not repair code, create a PR, or retain state outside Issues and two labels.

## Owned files

- `.github/workflows/ci-failure-issue.yml`
- `.github/ISSUE_TEMPLATE/ci-failure.md` if a fixed Issue body cannot be inline and stay readable
- `.github/labels.yml` only if the repository already manages labels declaratively; otherwise create the two labels manually before enabling the workflow

Do not add Rust modules, a database, a ledger, a new action, or a reusable automation framework.

## Steps

### 1. Define the exact workflow trigger and permission envelope

- Use `workflow_run` for completed `CI` workflow runs.
- Return immediately unless `conclusion == failure` and the run head branch is `main`.
- Grant exactly `contents: read` and `issues: write`.
- Add manual dispatch with a supplied run ID only if required to validate safely; do not make it a general log processor.

**Verify:** inspect rendered workflow permissions and event filters. Confirm it has no checkout, secrets, artifact download, `pull_request_target`, PR, release, or workflow-write authority.

### 2. Produce a bounded metadata-only fingerprint

- Fetch only workflow-run jobs metadata through GitHub API.
- Select the first failed job and failed step using explicit fields.
- Compose `ci:<head_sha>:<job_name>:<step_name>` after normalizing whitespace and bounding each field.
- Do not fetch or parse log text.

**Verify:** fixture/pure-shell test with two identical metadata payloads yields the same fingerprint; a changed failed step yields a different one. Confirm strings cannot become shell input.

### 3. Deduplicate through GitHub Issue search

- Search open Issues for the exact fingerprint marker in a fixed title prefix.
- If found, add a fixed-format comment containing only the new run URL and immutable SHA.
- Otherwise create an Issue with labels `automation:ci-failure` and `needs-human-triage`, fixed reproduction text `make ci`, and the bounded metadata body.

**Verify:** controlled dispatch against the same run twice creates one Issue and one follow-up comment. Inspect both bodies for absence of raw CI log data and runner/private paths.

### 4. Run in report-only mode for two scheduled cycles

- Keep the workflow limited to Issue creation/comments.
- Review all generated Issues for usefulness, duplicate suppression, and privacy.
- Do not add repair PR automation during this experiment.

**Verify:** record each cycle's result in the Issue itself. At the end, explicitly choose retain or delete.

## Automated quality boundary

The existing `CI` workflow and `make ci` remain the quality contract. This experiment consumes a CI result but does not rerun or replace checks. A human reproduces the failure before any repair.

## Manual review checklist

- Action references are immutable SHAs or no third-party action is used.
- No source checkout occurs.
- No raw logs/artifacts/issue body are sent to shell commands.
- The workflow cannot create branches, PRs, releases, tags, or commits.
- Deleting the workflow and two labels removes the experiment completely.

## Out of scope

Automated code repair, agents, ledgers, custom Rust binaries, dependency/security repair, release repair, and non-`main` CI failures.
