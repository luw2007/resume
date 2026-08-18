# Disposable CI-failure Issue experiment plan

## Outcome

Implement one report-only workflow: a failed trusted `main` CI run opens one fixed-format Issue for its commit or adds one fixed-format run-attempt comment to that Issue. A human decides whether and how to repair. The experiment expires after fourteen days.

## Scope limit

One new file:

```text
.github/workflows/ci-failure-issue.yml
```

Two labels are created manually before activation:

```text
automation:ci-failure
needs-human-triage
```

Do not add Rust code, an action, template, label manifest, ledger, database, script, artifact, checkout, repair agent, branch, or PR workflow.

## Implementation steps

### 1. Create labels and pilot controls outside the repository

Before merging the workflow, a maintainer creates the two labels manually and records outside the repository:

- pilot owner;
- start and mandatory deletion date, fourteen days later;
- maximum Issue/comment budget;
- every pilot-created Issue URL;
- acceptance that Issues/comments/notifications may persist after removal.

**Verify:** labels exist before activation; no workflow can create missing labels.

### 2. Add one metadata-only `workflow_run` notifier

**File:** `.github/workflows/ci-failure-issue.yml`.

- Trigger only on completed runs of the exact existing `CI` workflow.
- Do not add `workflow_dispatch`; arbitrary run-ID replay would add an Actions API and trust path.
- Set exact top-level permissions:

  ```yaml
  permissions:
    contents: read
    issues: write
  ```

- Use one Ubuntu job and one job-level guard. Require failed conclusion, `head_branch == main`, payload repository and head repository both equal the current repository, and upstream event in `{push, schedule}`.
- Add workflow-level concurrency keyed by validated repository ID plus head SHA with `cancel-in-progress: false`.
- Do not checkout, restore cache, download artifact, call Actions jobs/logs/artifacts APIs, invoke repository scripts, expose secrets, invoke an agent, or reference third-party actions.

**Verify:** YAML review proves no event other than `workflow_run`, no `pull_request_target`, no checkout/actions/artifacts/cache, and no scope beyond `contents: read` plus `issues: write`.

### 3. Build only the fixed run-level marker

- Validate head SHA as exactly 40 lowercase hex characters.
- Validate numeric run ID and attempt before constructing a URL.
- Build marker `ci-pilot:<head_sha>`.
- Construct the run URL from fixed repository identity and numeric run ID.
- Do not fetch job/step names: that would need an Actions API read and add arbitrary text to the Issue.

**Verify:** pure fixture values prove same SHA produces the same marker and a malformed identifier is a no-op. No payload field is interpolated into shell source, command text, filename, or YAML expression that evaluates as code.

### 4. Deduplicate strictly within the pilot's honest boundary

- List at most 100 Issues with the manually-created `automation:ci-failure` label, including open and closed results.
- Compare exact marker text in title/body using fixed-string matching; exclude pull requests returned by the Issues endpoint.
- If no marker exists, create one Issue with both labels.
- If the marker exists, compare a fixed `ci-pilot-run:<run_id>:<attempt>` comment marker; post one fixed comment only when absent.
- Never reopen, close, assign, mention, project-link, milestone, or otherwise modify an Issue.
- If the labeled Issue set reaches 100 or any API result is malformed, fail closed without writing.

**Verify:** controlled fixture/review covers new marker, repeated delivery, rerun attempt, closed marker, malformed response, and bounded-list overflow. The expected unit is one Issue per failed head SHA; no cross-commit root-cause deduplication is claimed.

### 5. Use a fixed-format body

Issue body contains only the marker, validated SHA, numeric run ID/attempt, constructed run URL, and:

```text
Maintainer starting point: make ci; reproduction not yet confirmed.
```

A comment contains only its run-attempt marker and constructed run URL. Raw logs remain in Actions. No job/step names, logs, artifacts, paths, environment data, URLs from payloads, actor names, commit messages, source content, or user Issue content enters either write.

**Verify:** inspect generated request payloads before enabling. Confirm no raw CI text can reach GitHub Issues.

### 6. Run and delete the pilot

- Observe only two weekly cycles.
- Stop immediately on duplicate, privacy incident, unexpected downstream Issue automation, budget breach, request for broader capability, or other untrusted-content exposure.
- At deadline, delete the workflow and two labels. Remove pilot Issues/comments where policy permits; otherwise close each with a fixed pilot-removal note and retain the external pilot URL list.
- Retain only after explicit human review finds at least one generated Issue useful enough to guide a maintainer to the linked CI run.

**Verify:** deletion plan is scheduled before activation. No useful human-triaged Issue by the deadline means delete.

## Current-workflow alignment

- `.github/workflows/ci.yml` is named `CI`, runs on `main` pushes and weekly schedule, and already defines the `make ci`-equivalent quality checks.
- `.github/workflows/release.yml` and `release-builds.yml` are excluded: they have release/artifact responsibilities outside this pilot.
- Existing Issue forms use manually maintained labels; no declarative label infrastructure exists, so adding it would violate the disposable scope.

## Planning review reconciliation

The workflow-planning worker proposed a job/step fingerprint by calling the
Actions jobs endpoint. The adversarial review rejected that path: it expands
the data/permission surface and places arbitrary workflow display text into
Issues. This plan intentionally retains only the run-level SHA marker,
excludes manual dispatch, and treats one failing commit—not an inferred root
cause—as the deduplication unit. The experiment is **no-go** if that reduced
metadata cannot support human triage.

## Explicitly deferred

Automated repair, draft PRs, agents, custom source modules, durable ledgers, cross-commit root-cause deduplication, user-Issue ingestion, dependency/security/release repair, flaky PTY diagnosis, and external-agent format compatibility repair.
