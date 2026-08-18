# Deletion-risk review: disposable CI-failure notifier

## Verdict

**No-go as currently specified.** A narrowly reduced metadata-only notifier can be defensible for a two-week pilot, but the frozen design and plan contain three blocking contradictions:

1. They require failed job and step names, but those are not present in the `workflow_run` event payload. Fetching the jobs endpoint requires Actions read access, which exceeds the frozen `contents: read` / `issues: write` permission envelope.
2. The proposed fingerprint includes the head SHA. The same failure on each new commit therefore produces a new fingerprint and a new Issue; it does not deduplicate a recurring failure across commits.
3. Deleting the workflow and labels does not delete Issues or comments already created. The experiment is not fully removable unless pilot-created Issues/comments are explicitly closed or deleted as part of teardown.

Proceed only with the reduced design and checklist below. Do not solve these blockers by adding an agent, checkout, logs, artifacts, a database, a repository ledger, source code, broader permissions, or repair automation.

## Evidence and trust boundary

The current repository establishes these relevant facts:

- `.github/workflows/ci.yml` is named `CI`, runs for pushes to `main`, pull requests, a weekly schedule, manual dispatch, and published releases, and currently grants only `contents: read`.
- The CI job and step names are currently static workflow text, but CI executes repository code after checkout. A downstream `workflow_run` workflow must therefore treat the upstream run as potentially hostile even when the downstream workflow itself has a write-capable token.
- `.github/workflows/release.yml` has elevated release and external-repository behavior, including `contents: write`, attestations, OIDC, a Homebrew PAT, artifact downloads, and a push. The notifier must never broaden its workflow-name filter or become a reusable observer of arbitrary workflows.
- `.github/workflows/release-builds.yml` produces artifacts. The notifier has no reason to read them.
- `.github/ISSUE_TEMPLATE/bug.yml` warns against session transcripts, credentials, tokens, private keys, and sensitive remote URLs. Automated Issues bypass that human confirmation, so the notifier must enforce privacy structurally rather than rely on the form.
- `src/diagnostics.rs` contains application redaction logic, but importing, invoking, or duplicating it would add source coupling and still would not make arbitrary logs safe. The notifier should never ingest sensitive text in the first place.

GitHub Actions mechanics relevant to the review:

- A `workflow_run` workflow runs from the default branch and can receive secrets and a write-capable `GITHUB_TOKEN` even when the triggering workflow could not. GitHub explicitly warns that running untrusted code or processing untrusted artifacts in this trigger can compromise the repository. The safety property here must come from no checkout, no artifact/log ingestion, no dynamic code execution, and an explicit minimal permission block—not merely from the upstream CI permission block.
- The `workflow_run` payload contains run-level metadata, not the complete jobs/steps collection. The REST endpoint used to list jobs for a run is an Actions API endpoint and requires Actions read permission for fine-grained tokens.
- Setting any permissions explicitly makes unspecified `GITHUB_TOKEN` permissions `none`. Thus a workflow with only `contents: read` and `issues: write` must not assume it can list run jobs.
- `workflow_run.workflows: [CI]` selects by workflow name, while event-level `types: [completed]` does not itself prove that the run was a trusted `main` run. Job-level validation remains necessary.
- GitHub Issue search is indexed rather than a transactional uniqueness constraint. Two invocations can both observe no match and create duplicates unless serialized.

References: GitHub Docs, “Events that trigger workflows” (`workflow_run`); “Workflow syntax for GitHub Actions” (`permissions`, `concurrency`); REST API, “Workflow runs / List jobs for a workflow run”; REST API, “Issues” and “Search.”

## Findings and minimum constraints

### 1. Job/step extraction forces forbidden Actions access — blocker

**Risk:** The plan says to fetch workflow-run jobs metadata and select a failed job and step. That changes the notifier from event-payload processing into an Actions API reader. `actions: read` is outside the permitted envelope. Adding it also makes future log/artifact expansion easier.

**Smallest remedy:** **Delete failed job and failed step from the fingerprint and Issue body.** Use only fields already present in the validated event payload, such as the immutable head SHA and run ID/attempt. Do not add `actions: read`, and do not call any Actions jobs, logs, or artifacts endpoint.

### 2. The proposed fingerprint guarantees cross-commit Issue churn — blocker

**Risk:** `CI + head SHA + job + step` changes on every commit. It can deduplicate a rerun of one commit, but not the same failing check on successive commits. On an actively broken `main`, every push can create another Issue. Conversely, using only mutable names can combine unrelated failures.

**Smallest remedy:** Be honest about the unit: one open Issue per failed **run/head SHA**, not one Issue per root cause. Fingerprint with a fixed machine marker based on repository plus workflow identity plus run ID (or head SHA if reruns must share an Issue). For the pilot, cap noise operationally: at most one created Issue per run/head SHA, no more than one bot comment per run attempt, and pause/delete the workflow immediately after the first duplicate or unacceptable burst. Do not invent cross-commit “smart” deduplication; that requires data the experiment forbids.

### 3. Issue search is not atomic

**Risk:** Search indexing lag and concurrent deliveries/reruns can both see no existing Issue and create duplicates. Searching for arbitrary title text is also vulnerable to normalization, truncation, and query-syntax mistakes.

**Smallest remedy:** Add workflow-level `concurrency` keyed only from validated, bounded payload identifiers (for example repository ID + workflow ID + head SHA), with `cancel-in-progress: false`. List open Issues carrying `automation:ci-failure` and compare an exact fixed marker locally; do not rely on free-text search semantics. Keep the candidate set bounded and fail closed if pagination would be required. If this cannot be implemented without shell interpolation, delete update/dedup behavior and create at most one controlled pilot Issue manually.

### 4. “Delete workflow and labels” leaves durable records — blocker

**Risk:** Created Issues, comments, notifications, email copies, audit events, and downstream integrations survive workflow/label deletion. Removing a label does not remove an Issue. Closing Issues still preserves their contents. Public content may be cached externally.

**Smallest remedy:** Define teardown before activation: record the exact pilot Issue URLs in the maintainer’s temporary pilot notes (not a repository ledger), then delete bot comments and delete Issues where repository policy/API permits; otherwise close them with a fixed “pilot removed” note and accept that full erasure is impossible. Delete both labels and the workflow. If permanent public Issue history is unacceptable, **do not run the pilot**.

### 5. `workflow_run` is a privileged boundary

**Risk:** The downstream token can write Issues even if upstream CI came from a context with reduced permissions. Any checkout, artifact download, log parsing, cache restoration, or execution of upstream-controlled text would create a privilege bridge.

**Smallest remedy:** One job, no checkout, no cache, no artifacts, no logs, no repository scripts, no generated scripts, no `eval`, and no command assembled from event fields. Use only GitHub’s API to read/write Issues. Pin any action to a full commit SHA; preferably use no third-party action. The explicit workflow/job permissions must remain exactly `contents: read` and `issues: write`, with all others absent/none. Never add secrets.

### 6. `main` branch alone does not identify the intended event

**Risk:** Current `CI` also accepts `workflow_dispatch`, schedule, release, and pull-request events. A check of only `head_branch == main` can admit more than trusted main pushes and the intended weekly run, depending on payload details. A same-name workflow or future trigger change can silently widen coverage.

**Smallest remedy:** Validate all of the following before any write:

- `workflow_run.conclusion == 'failure'`;
- exact workflow ID (preferred) or exact path/name for the existing `.github/workflows/ci.yml` / `CI`;
- `workflow_run.head_branch == 'main'`;
- `workflow_run.repository.id == github.repository_id` and head repository is the same repository;
- upstream event is an explicit allowlist: `push` and `schedule` only for this pilot.

Fail closed when a field is absent or unexpected. Pull requests, releases, and both upstream and downstream manual dispatches should be excluded.

### 7. Manual dispatch is unnecessary extra authority

**Risk:** A dispatch input containing a run ID creates a second code path, requires refetching Actions metadata, invites arbitrary-run selection, and complicates the permission and trust proof. Validation conveniences tend to become permanent operational interfaces.

**Smallest remedy:** **Delete `workflow_dispatch`.** Validate with a temporary, maintainer-controlled failing commit/run or by reviewing a fixture outside the deployed workflow. Do not grant `actions: read` to support testing.

### 8. Metadata can become content injection or a privacy leak

**Risk:** Workflow/job/step names are not inherently secret-safe. Future workflow edits could put paths, URLs, issue mentions, Markdown, control characters, or attacker-chosen text in names. Raw `html_url` and other URL fields should not be accepted blindly. The application’s Rust redactor does not cover this workflow and is not a justification to ingest data.

**Smallest remedy:** Persist only fixed prose plus strictly validated primitive identifiers: hexadecimal SHA, numeric run ID/attempt, and a run URL constructed from the fixed current repository identity and numeric run ID. Omit job/step names and arbitrary workflow metadata. Disable mention effects by never copying arbitrary text. Build API JSON with a real JSON encoder/API client, never string concatenation or shell interpolation.

### 9. The reproduction claim can be misleading

**Risk:** `make ci` is described as a fixed reproduction identifier, but current `.github/workflows/ci.yml` directly runs cargo commands on Ubuntu and macOS and includes CI-only environment isolation. A generic `make ci` may not reproduce platform-, schedule-, or environment-specific failures. A false reproduction claim makes Issues noisy rather than actionable.

**Smallest remedy:** Phrase it as “maintainer starting point: `make ci`; reproduction not yet confirmed,” never as a verified reproduction. If maintainers cannot diagnose from the linked run without copying logs into the Issue, delete the pilot rather than expanding ingestion.

### 10. Labels and Issue policy can become permanent infrastructure

**Risk:** Declarative label files, custom templates, source modules, bot frameworks, ledgers, and reusable actions turn a disposable trial into maintained product surface. Label creation may also fail after workflow activation, producing partial behavior.

**Smallest remedy:** Create exactly `automation:ci-failure` and `needs-human-triage` manually before enabling. Do not add `.github/labels.yml`, a CI-failure Issue template, a reusable action, Rust code, database, ledger, project board, milestone, assignee automation, or external service. Missing labels must fail the workflow closed; it must not create or mutate labels.

### 11. Comments and notifications can amplify noise

**Risk:** Every rerun can generate a comment, notifications, webhook traffic, and external bot reactions. A run URL already in an Issue can be posted repeatedly. Automated Issue creation can also trigger other repository workflows or integrations not visible in these files.

**Smallest remedy:** Before commenting, compare a fixed run-attempt marker against existing bot comments and no-op if present. Never mention users/teams, assign, milestone, project-link, reopen, or close Issues. Inventory repository rules/webhooks/GitHub Apps before activation. Stop on the first duplicate, unexpected downstream automation, or more than the pre-agreed Issue budget.

### 12. Open-only matching lets closure recreate noise

**Risk:** If deduplication searches only open Issues, a human closing a pilot Issue followed by a rerun creates a replacement. This undermines human suppression and leaves more durable records.

**Smallest remedy:** Treat an exact marker in either open or closed pilot Issues as already observed and no-op. Never reopen automatically. A human may deliberately remove the marker/Issue only as part of a new, separately approved experiment.

### 13. Workflow drift can silently broaden the pilot

**Risk:** A later edit can add checkout, logs, permissions, repair behavior, broader triggers, or third-party actions while retaining the same benign filename and labels. The notifier itself is repository code on the default branch.

**Smallest remedy:** Require maintainer review/CODEOWNERS protection for the workflow during the pilot and compare every deployed revision to this negative contract. Any requested capability expansion ends the experiment; it is not an in-place enhancement.

### 14. A two-week observation window has no automatic safe stop

**Risk:** “Observe for two weeks” does not stop execution after two weeks. A forgotten workflow becomes durable and continues creating Issues indefinitely.

**Smallest remedy:** Pre-schedule a maintainer removal change/date before enabling and set a hard run/Issue budget. Because runtime date logic adds complexity and can fail open, prefer deleting/disable-merging the workflow at the deadline. No decision by the deadline means delete, not retain.

## Reduced defensible notifier

The only defensible pilot surface is:

```text
completed CI workflow_run
  -> validate same repository, exact CI identity, event in {push, schedule},
     main, failure, bounded primitive identifiers
  -> serialize per head SHA
  -> create one fixed-format labeled Issue, or no-op/comment once for a new
     run attempt
```

The Issue may contain only:

- a fixed machine marker derived from repository/workflow/run or head SHA;
- the validated 40-character commit SHA;
- numeric run ID and attempt;
- a run URL constructed from the fixed repository and numeric run ID;
- fixed text: “Maintainer starting point: `make ci`; reproduction not yet confirmed.”

It must not contain workflow/job/step display names, logs, artifacts, environment data, runner paths, arbitrary URLs, event text, commit messages, actor names, Issue text, or source-derived content.

## Two-week report-only pilot: go/no-go checklist

All boxes are mandatory before **go**.

### Contract and permissions

- [ ] One new workflow only; no source, template, label manifest, action, database, ledger, or framework files.
- [ ] Trigger is only `workflow_run` for completed exact `CI`; no `workflow_dispatch`, `pull_request_target`, or reusable caller.
- [ ] Job validates failure, `main`, same repository/head repository, exact CI identity, and upstream event allowlist `{push, schedule}` before writes.
- [ ] Explicit permissions are only `contents: read` and `issues: write`; no `actions: read`, secrets, OIDC, PR, workflow, release, package, deployment, or security permissions.
- [ ] No checkout, cache, artifacts, logs, source execution, repository scripts, external network service, or agent/model.
- [ ] Any referenced action is first-party and pinned to a full immutable commit SHA; no floating tags.

### Data and behavior

- [ ] Job/step names have been deleted from the design because obtaining them violates the permission envelope.
- [ ] Only validated SHA/numeric identifiers, constructed run URL, and fixed prose enter Issues/comments.
- [ ] API payloads use JSON encoding; no event value is interpolated into shell code, command lines, expressions that form code, or filenames.
- [ ] Concurrency serializes the deduplication unit; exact-marker matching covers open and closed pilot Issues.
- [ ] A previously observed run attempt is a no-op; closed Issues are never reopened or replaced automatically.
- [ ] Exactly two labels exist before enablement and the workflow cannot create labels.
- [ ] No mentions, assignments, milestones, projects, reactions, closure, or PR creation.
- [ ] Repository rules, Apps, webhooks, and other workflows have been checked for Issue-created/comment side effects.

### Pilot limits and teardown

- [ ] Maintainer accepts that public Issue content may remain recoverable/cached after deletion.
- [ ] A hard maximum Issue/comment budget and “first duplicate/privacy incident stops pilot” rule are recorded outside the repository.
- [ ] Start date, deletion date (14 days later), owner, and removal change are scheduled before enablement.
- [ ] Teardown includes workflow deletion, both label deletions, and deletion where possible—or closure with explicit acknowledgement of retained history—of every pilot Issue/comment.
- [ ] No useful human-triaged Issue by the deadline means delete.
- [ ] Any request for logs, artifacts, checkout, broader permissions, repair PRs, agents, state, or source code means **no-go** for this experiment and requires a new design review.

## Final recommendation

**No-go for the plan as written. Conditional go only after deleting job/step collection and manual dispatch, narrowing event validation, serializing exact-marker deduplication, accepting the limited one-run/head-SHA noise model, and precommitting to teardown of the Issues themselves.** If the reduced run-level metadata is not actionable, delete the notifier rather than making it more privileged or durable.
