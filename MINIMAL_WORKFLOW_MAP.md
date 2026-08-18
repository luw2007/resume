# Minimal Workflow Map — disposable CI-failure Issue experiment

Attempt: `workflow-a1` · Node: `MinimalWorkflowMap` (`.dispatch/LEDGER.md`)
Scope: **plan only**. No application, workflow, or label state is changed by this report.

The whole experiment is **one new file** plus **two labels**:

| Artifact | Kind | Created by |
|---|---|---|
| `.github/workflows/ci-failure-issue.yml` | one workflow file, ~90 lines, inline `bash` + `gh` | this experiment |
| `automation:ci-failure` | repo label | manual, once |
| `needs-human-triage` | repo label | manual, once |

Nothing else. No Rust module, no action, no composite action, no `.github/labels.yml`
(the repo does **not** manage labels declaratively today — issue forms hardcode
`type:bug` / `type:feature` / `type:question` at `.github/ISSUE_TEMPLATE/bug.yml:4`,
`feature.yml:4`, `question.yml:4`), no ledger, no database, no checkout, no artifact.

---

## 1. Observed repository facts (cited)

These are the only inputs the design depends on; each is verified in this worktree.

| Fact | Location |
|---|---|
| Upstream workflow is literally named `CI` | `.github/workflows/ci.yml:1` |
| It runs on `push` to `main`, `pull_request`, weekly `schedule` (`17 5 * * 1`), `workflow_dispatch`, and `release: published` | `.github/workflows/ci.yml:3-11` |
| It holds `permissions: contents: read` only | `.github/workflows/ci.yml:13-14` |
| Job names that can appear in a fingerprint: `Quality (ubuntu-latest)`, `Quality (macos-latest)`, `Rust 1.91 MSRV`, `Dependency policy` | `.github/workflows/ci.yml:22`, `:73`, `:81` |
| Failing step names are stable strings: `Run cargo fmt --check`, `Run cargo clippy ...`, `Run cargo test ...`, `Benchmark suite compiles and runs (smoke, not full statistical sampling)`, `Rustdoc warnings are errors`, `Isolate agent and XDG roots` | `.github/workflows/ci.yml:33`, `:64-70` |
| Local reproduction identifier is real: `ci: fmt lint test msrv deny` | `Makefile:46` |
| Other workflows exist and **must not** be observed | `.github/workflows/release.yml`, `.github/workflows/release-builds.yml` |
| Repo is `luw2007/resume` | `git remote -v` |
| Security reports go to a private advisory, not Issues | `.github/ISSUE_TEMPLATE/config.yml:4-5` |

Matrix job names carry the OS (`Quality (ubuntu-latest)` vs `Quality (macos-latest)`),
so an ubuntu-only failure and a macos-only failure at the same SHA and step are
distinct fingerprints. That is intended: they are different failures to triage.

---

## 2. Event filter — exact

```yaml
name: CI failure issue

on:
  workflow_run:
    workflows: ["CI"]        # exact `name:` at .github/workflows/ci.yml:1
    types: [completed]
  workflow_dispatch:
    inputs:
      run_id:
        description: "Existing CI run ID to replay (validation only)"
        required: true
        type: string
```

### 2.1 Event semantics this relies on

1. **The notifier file must live on the default branch.** GitHub only triggers a
   `workflow_run` workflow if the workflow file is on the repository default branch
   ([Events that trigger workflows](https://docs.github.com/actions/using-workflows/events-that-trigger-workflows)).
   Consequence: this cannot be validated from a feature branch by pushing; that is why
   §7 uses `workflow_dispatch` for the test path instead. This is also a *deletion*
   property — remove the file from `main` and the trigger ceases immediately.
2. **`workflow_run` runs always execute the default-branch copy of the notifier, with
   the base repository's `GITHUB_TOKEN` and access to secrets** (same source; also the
   documented reason `workflow_run` is a privileged trigger). The notifier therefore
   never runs attacker-authored notifier code. It compensates for the privilege by
   requesting the minimum token scope (§3) and by never checking out or executing
   anything from the observed run.
3. **`workflow_run` fires for *every* completion of `CI`**, including `pull_request`
   runs from forks, `release` runs, and `workflow_dispatch` runs. `workflows: ["CI"]`
   filters by workflow *name*, not by trigger. All trigger discrimination must happen
   in an `if:` (§2.2). There is no `branches:` filter that is safe to rely on alone,
   because a fork PR run's `head_branch` is the *fork's* branch name and could be
   literally `main`.
4. **No self-recursion.** `workflows: ["CI"]` excludes this workflow's own name, so
   the notifier cannot observe itself. Independently, events raised via `GITHUB_TOKEN`
   (the Issue create/comment) do not trigger further workflow runs.

### 2.2 Trust gate — single job-level `if`

```yaml
permissions:
  contents: read
  issues: write

concurrency:
  group: ci-failure-issue-${{ github.event.workflow_run.head_sha || inputs.run_id }}
  cancel-in-progress: false

jobs:
  notify:
    name: Open or update CI failure issue
    runs-on: ubuntu-latest
    if: >-
      github.event_name == 'workflow_dispatch' ||
      ( github.event.workflow_run.conclusion == 'failure' &&
        github.event.workflow_run.head_branch == 'main' &&
        github.event.workflow_run.head_repository.full_name == github.repository &&
        contains(fromJSON('["push","schedule","workflow_dispatch"]'),
                 github.event.workflow_run.event) )
```

Each conjunct earns its place:

| Conjunct | Rejects |
|---|---|
| `conclusion == 'failure'` | `success`, `cancelled`, `skipped`, `timed_out`, `action_required`, `neutral`, and `null` (still running) |
| `head_branch == 'main'` | topic branches, tags, release refs |
| `head_repository.full_name == github.repository` | **every fork run**, including a fork whose branch is named `main` |
| `event ∈ {push, schedule, workflow_dispatch}` | `pull_request` (untrusted head, even same-repo PRs are pre-review) and `release` (release failures are a different escalation path, `.github/workflows/release.yml`) |

`concurrency` with `cancel-in-progress: false` serialises the three CI jobs that can
fail at the same SHA (`Quality`, `msrv`, `deny`). Without it, two simultaneous
completions could both read "no open Issue" and both create one. Grouping on
`head_sha` is exactly right, because the head SHA is part of the fingerprint (§4):
two runs that could collide are precisely two runs sharing a SHA.

---

## 3. Permission envelope

```yaml
permissions:
  contents: read
  issues: write
```

Declared at workflow top level, so every unlisted scope is set to `none` for the whole
`GITHUB_TOKEN`. Explicitly **absent**:

- `pull-requests` → cannot open, comment on, or merge a PR
- `contents: write` → cannot push a commit, branch, or tag
- `actions: write` → cannot re-run, cancel, or delete runs (read is not needed either; see §4.1)
- `packages`, `deployments`, `id-token`, `security-events`, `statuses`, `checks`, `pages`
- `secrets` are never referenced; no `secrets.*` expression appears in the file
- no `pull_request_target` trigger anywhere
- no `actions/checkout`, no `actions/download-artifact`, no third-party action **at all**,
  so there is no action SHA to pin and no supply-chain review surface

`contents: read` is retained only because it is the repo-wide convention
(`.github/workflows/ci.yml:13-14`, `release.yml:8-9`) and because `gh` prefers a
token that can resolve the repository. It grants no write authority.

---

## 4. GitHub API calls — the complete list

Exactly **three** endpoint families are touched. Everything uses the preinstalled
`gh` CLI on `ubuntu-latest`; no `curl`, no jq-less parsing, no extra install step.

### 4.1 Read failed job + step (1 call)

```
GET /repos/{owner}/{repo}/actions/runs/{run_id}/jobs?filter=latest&per_page=100
```

`filter=latest` returns only the most recent attempt of each job, so a re-run does not
resurrect a previously-failed attempt
([REST API endpoints for workflow jobs](https://docs.github.com/en/rest/actions/workflow-jobs)).
The endpoint is readable by *anyone with read access to the repository*, which is why
`actions: read` is not required in §3.

**Fields consumed — and only these:**

| Field | Use |
|---|---|
| `jobs[].name` | fingerprint component, Issue body |
| `jobs[].conclusion` | select the failed job (`== "failure"`) |
| `jobs[].steps[].name` | fingerprint component, Issue body |
| `jobs[].steps[].conclusion` | select the failed step (`== "failure"`) |
| `jobs[].steps[].number` | tie-break ordering only; **not** in the fingerprint |

`conclusion` is documented as `success | failure | neutral | cancelled | skipped |
timed_out | action_required | null`, and `steps[]` as objects with
`{status, conclusion, name, number, started_at, completed_at}` (same source). No other
field is read. Specifically **not** read: `html_url` of the job, `runner_name`,
`runner_group_name`, `labels`, `check_run_url`, `node_id`, timing fields, or
`jobs[].steps[]` of non-failed jobs.

Zero calls are made to logs. `GET /actions/jobs/{job_id}/logs` and
`GET /actions/runs/{run_id}/logs` are **never** invoked; neither is
`/actions/runs/{run_id}/artifacts`. That is the hard boundary the experiment is
testing, so it is enforced by absence, not by redaction.

Extraction is a single `--jq` program, so no log text can ever reach the shell:

```bash
gh api "repos/$GH_REPO/actions/runs/$RUN_ID/jobs?filter=latest&per_page=100" \
  --jq '[ .jobs[]
          | select(.conclusion == "failure")
          | { job: .name,
              step: ( [ .steps[]? | select(.conclusion == "failure") ]
                      | sort_by(.number) | first | .name // "unknown-step" ) } ]
        | first // empty' > job.json
```

Bounded by construction: at most one object, two string fields.

For `workflow_dispatch`, `RUN_ID` comes from `inputs.run_id` after a strict
`^[0-9]{1,20}$` regex; for `workflow_run` it is `github.event.workflow_run.id`. In
both cases the value reaches `bash` through `env:`, never through `${{ }}` string
interpolation inside `run:`, so no expression injection is possible.

### 4.2 Dedupe lookup (1 call) — REST list, *not* code search

```
GET /repos/{owner}/{repo}/issues?state=open&labels=automation:ci-failure&per_page=100
```

Consumed fields: `number`, `title`, and `pull_request` (presence means it is a PR, so
skip it — the Issues endpoint returns both).

**Why not `gh search issues` / `GET /search/issues`?** The search index is
asynchronous and rate-limited at 30 req/min. A freshly created Issue is frequently not
findable for tens of seconds. Two CI jobs failing at the same SHA minutes apart would
each see an empty result and each create an Issue — a duplicate, which is exactly the
failure mode listed in the delete criteria. The label-scoped list endpoint reads the
primary datastore and is read-your-writes consistent. The label filter keeps the page
small: the only Issues carrying `automation:ci-failure` are the ones this workflow
created, so `per_page=100` without pagination is sufficient in practice, and the
experiment is deleted long before 100 open automation Issues could accumulate.

`concurrency` (§2.2) plus this consistent read is the complete dedupe story. There is
no lock file, no cache, no ledger.

### 4.3 Write — exactly one of two (1 call)

```
POST /repos/{owner}/{repo}/issues                              # new fingerprint
POST /repos/{owner}/{repo}/issues/{issue_number}/comments      # known fingerprint
```

Both via `gh issue create` / `gh issue comment` with `--body-file`, never `--body`
with an interpolated string. Labels are attached in the same create call, so no
separate `POST /issues/{n}/labels` is needed.

Total: **3 API calls per qualifying failure.** No pagination loop, no retry storm.

---

## 5. Fingerprint, title, body — deterministic

### 5.1 Fingerprint

```
ci:<head_sha>:<job_slug>:<step_slug>
```

Normalisation, applied identically to `job` and `step`, in a pure function of the two
input strings:

1. lowercase (`tr '[:upper:]' '[:lower:]'`)
2. collapse every run of characters outside `[a-z0-9]` to a single `-` (`tr -cs 'a-z0-9' '-'`)
3. strip leading/trailing `-`
4. truncate to 40 bytes, strip a trailing `-` again
5. empty → `unknown`

`head_sha` is the 40-char lowercase hex from the event payload; it is already
`[0-9a-f]{40}` and is passed through unchanged.

Worked examples from the real CI file:

| Job / step | Fingerprint tail |
|---|---|
| `Quality (ubuntu-latest)` / `Run cargo clippy --all-targets --all-features --locked -- -D warnings` | `quality-ubuntu-latest:run-cargo-clippy-all-targets-all-fea` → truncated to 40: `run-cargo-clippy-all-targets-all-feature` |
| `Quality (macos-latest)` / `Rustdoc warnings are errors` | `quality-macos-latest:rustdoc-warnings-are-errors` |
| `Rust 1.91 MSRV` / `Run cargo build --all-features --locked` | `rust-1-91-msrv:run-cargo-build-all-features-locked` |
| `Dependency policy` / `Run EmbarkStudios/cargo-deny-action@v2` | `dependency-policy:run-embarkstudios-cargo-deny-action-v2` |

Properties:

- **Deterministic.** Same two metadata payloads → byte-identical fingerprint. The
  function reads no clock, no run ID, no run number, no attempt number, no URL.
- **Discriminating.** Changing the failed step changes the fingerprint; changing the
  OS changes it via the matrix-qualified job name.
- **Shell-inert.** The output alphabet is `[a-z0-9:-]`. It cannot contain `$`, backtick,
  quote, newline, or `;`, so it is safe in a title, a body, and a `grep -F` pattern.
  It is nevertheless still passed via `env:` and `--body-file`, never interpolated.
- **Bounded.** ≤ `3 + 40 + 1 + 40 + 1 + 40 = 125` bytes.

Since the SHA is a component, a *new commit* that fails the same step gets a *new*
Issue. That is deliberate: a different commit is a different investigation. The
dedupe target is re-runs and multi-job failures of one commit, plus the weekly
`schedule` run, which re-tests the same `main` SHA and therefore re-comments rather
than re-filing. This is exactly acceptance criterion 2 in the design spec.

### 5.2 Title

```
CI failure: <job> / <step> @ <sha[0:7]> [<fingerprint>]
```

Fixed prefix `CI failure: ` makes it human-scannable; the bracketed fingerprint is the
machine key. `<job>` and `<step>` here are the *raw* names truncated to 48 bytes with
newlines stripped, purely for readability — matching never uses them. Worst case
length ≈ `12 + 48 + 3 + 48 + 2 + 7 + 2 + 125 = 247`, under the 256-char Issue title
limit. If any component overflows, the fingerprint is preserved and the readable
portion is trimmed first.

Dedupe test is an exact substring match on the bracketed key:

```bash
existing="$(jq -r --arg fp "[$FINGERPRINT]" \
  '[ .[] | select(has("pull_request") | not)
         | select(.title | contains($fp)) ] | first | .number // empty' issues.json)"
```

Brackets prevent a short fingerprint from matching inside a longer one.

### 5.3 Issue body — fixed template, six fields

```markdown
Automated report. Metadata only — no log text was read or copied.

- Fingerprint: `ci:<head_sha>:<job_slug>:<step_slug>`
- Commit: <head_sha>
- Workflow: CI
- Job: <job name>
- Step: <step name>
- Run: https://github.com/luw2007/resume/actions/runs/<run_id>

Reproduce locally:

    make ci

A maintainer must reproduce and fix this manually. This workflow does not
modify source, open pull requests, or re-run CI.

<!-- ci-failure-issue: automated, metadata only -->
```

Every value is one of: the 40-hex SHA, the literal `CI`, a job name from
`.github/workflows/ci.yml`, a step name from the same file, a numeric run ID, or the
literal `make ci`. Nothing else is interpolated. Not present, by construction: log
lines, `$RUNNER_TEMP` paths (the CI file builds an isolated `$RUNNER_TEMP/resume-ci-home`
tree at `.github/workflows/ci.yml:33-62` — those paths never surface here), environment
variables, session data, artifact contents, URLs harvested from output, or any string
the runner produced. `make ci` is a genuine target (`Makefile:46`), so the reproduction
instruction is not aspirational.

Job and step names are written into the body through `--body-file` from a heredoc with
a **quoted** delimiter (`<<'EOF'` is not usable with substitution, so values are
injected with `envsubst`-free `printf '%s'` into pre-split fixed segments). Newlines in
a name are stripped during the same truncation pass used for the title.

### 5.4 Comment body — fixed, two fields

```markdown
Same fingerprint failed again.

- Commit: <head_sha>
- Run: https://github.com/luw2007/resume/actions/runs/<run_id>
```

Nothing more. No counters, no timestamps beyond GitHub's own comment metadata, no
mutation of the original Issue body — so there is no accumulating state inside the
Issue either.

---

## 6. Labels

Two labels, created **manually once** before enabling the workflow:

| Label | Colour suggestion | Purpose |
|---|---|---|
| `automation:ci-failure` | `#ededed` | machine marker; also the dedupe query filter (§4.2) |
| `needs-human-triage` | `#d93f0b` | states plainly that no agent will act |

Applied only at Issue creation, in the `gh issue create --label` flags. The workflow
never creates a label, never edits a label, never removes one, and never touches the
existing `type:bug` / `type:feature` / `type:question` labels used by the issue forms.
If either label is missing, `gh issue create` fails loudly — an acceptable, visible
failure mode for a two-week experiment, and better than silently filing unlabelled
Issues that the dedupe query would then never find.

`automation:ci-failure` doing double duty as the dedupe filter is what makes label
deletion a real off-switch: with the label gone, creation errors out rather than
degrading into duplicate-Issue spam.

---

## 7. Manual dispatch test

`workflow_dispatch` with a required `run_id` is the only validation path, because a
`workflow_run` workflow will not fire until its file is on `main` (§2.1).

Guards on the dispatch path:

1. `run_id` must match `^[0-9]{1,20}$`, else fail before any API call.
2. Re-fetch the run and re-apply the trust gate server-side rather than trusting the
   dispatcher: `GET /repos/{owner}/{repo}/actions/runs/{run_id}` with
   `--jq '{name, conclusion, head_branch, event, head_sha, repo: .head_repository.full_name}'`,
   then require `name == "CI"`, `conclusion == "failure"`, `head_branch == "main"`,
   `repo == github.repository`, `event ∈ {push, schedule, workflow_dispatch}`. Identical
   predicate to §2.2. This is the *only* extra API call, and it exists solely on the
   dispatch path.
3. `workflow_dispatch` requires `actions: write` on the *caller*, so only maintainers
   can invoke it. It accepts a run ID, never a log URL, a file, or free text — it is
   not a general-purpose log processor.

Acceptance cases, each mapping to a spec criterion:

| # | Procedure | Expected | Spec criterion |
|---|---|---|---|
| A1 | Dispatch with a known-failed `main` CI run ID | one Issue, correct fingerprint, both labels | 1 |
| A2 | Dispatch the **same** run ID again | zero new Issues, exactly one new comment | 2 |
| A3 | Dispatch a **successful** run ID | job exits `0` at the guard, no Issue, no comment | — |
| A4 | Dispatch a fork **pull_request** CI run ID | rejected at the guard, no Issue | trust gate |
| A5 | Dispatch a `release`-triggered CI run ID | rejected at the guard | trust gate |
| A6 | Fingerprint unit check: run the normalisation snippet over two identical payloads, then over one with a changed step name | identical, then different | design §fingerprint |
| A7 | Read A1's Issue body end to end | no log text, no `$RUNNER_TEMP` path, no env var, no secret | 3 |
| A8 | Re-read the rendered `permissions:` block on the run page | `contents: read`, `issues: write`, everything else `none` | 4 |
| A9 | Follow the run link from the Issue; run `make ci` locally | run is inspectable; `make ci` is a real target (`Makefile:46`) | 5 |
| A10 | Two CI jobs fail at the same SHA (e.g. `Quality (ubuntu-latest)` and `Rust 1.91 MSRV`) | two Issues — distinct fingerprints — and this is correct, not a duplicate | 1, 2 |

A6 is runnable locally against a saved JSON fixture with no network and no repo
mutation, which keeps the "pure-shell test" requirement from the plan honest.

---

## 8. Deletion

```bash
git rm .github/workflows/ci-failure-issue.yml && git commit && git push origin main
gh label delete automation:ci-failure --yes
gh label delete needs-human-triage    --yes
```

That is the complete removal. It holds because:

- **The trigger is the file.** `workflow_run` only fires for a workflow file present on
  the default branch. Deleting it from `main` stops the trigger on the next event; there
  is no schedule, no webhook, no App installation, no deploy key, no repository
  dispatch, and no external service to deregister.
- **No code depends on it.** It adds no Rust module, no `Cargo.toml` entry, no `Makefile`
  target, no CI job in `ci.yml`, no test, no fixture, and no import. `make ci`
  (`Makefile:46`) neither invokes nor knows about it, so `cargo build` / `cargo test`
  are byte-identical before and after.
- **No state survives.** The only writes were Issues and comments. Issues are ordinary
  repository content a maintainer can close or leave; they are not a data structure any
  code reads. Nothing is stored in Actions cache, variables, secrets, artifacts,
  environments, or a branch. There is no schema and therefore no migration.
- **No permission residue.** Permissions were per-workflow `permissions:` keys, not
  repository or organisation settings. Deleting the file deletes the grant. Nothing was
  added to branch protection, rulesets, or Actions repository settings.
- **The labels are inert.** They are used by nothing else — the issue forms hardcode
  `type:*` labels (`.github/ISSUE_TEMPLATE/bug.yml:4` and siblings). Deleting them
  strips them from historical Issues and changes no behaviour.
- **Failure of deletion is loud, not silent.** Because `automation:ci-failure` is also
  the dedupe filter (§4.2, §6), deleting the labels but forgetting the file causes
  `gh issue create` to error, surfacing the leftover instead of quietly filing junk.

Reverting is `git revert` of one commit plus recreating two labels.

---

## 9. What this plan deliberately refuses

For the adversarial review at `DELETION_RISK_REVIEW.md` (`risk-a1`), the refusals are
enumerated so drift is detectable:

- no `actions/checkout` and no `persist-credentials`
- no artifact download, no log download, no log parsing, no redaction layer (nothing to redact)
- no third-party action, therefore no action-SHA pinning obligation and no supply chain
- no `pull_request_target`, no `secrets.*`, no `id-token`
- no PR, branch, tag, commit, release, or workflow-file write
- no re-run or cancel of CI (`actions: write` absent)
- no dedupe database, cache, ledger, or state branch — `concurrency` + a consistent
  REST read do the whole job
- no reusable workflow, no composite action, no "framework" for future automations
- no agent, model call, or execution of any string originating from a log, an Issue, or
  a model response
- no observation of user-created Issues, of non-`main` failures, or of `release.yml` /
  `release-builds.yml`
- no matrix, no multi-job structure — one job, one `runs-on: ubuntu-latest`, one
  `if:`, four `run:` steps, no `uses:` at all

## 10. Verification performed for this report

Read-only. `git status --short` is clean apart from this new file; no formatter,
linter, or project-wide suite was run, and no application or workflow file was
modified. Repository claims come from direct reads of `.github/workflows/ci.yml`,
`.github/workflows/release.yml`, `.github/ISSUE_TEMPLATE/*.yml`, `Makefile`, and
`git remote -v` in this worktree. GitHub event and API semantics are cited to
[Events that trigger workflows](https://docs.github.com/actions/using-workflows/events-that-trigger-workflows)
and [REST API endpoints for workflow jobs](https://docs.github.com/en/rest/actions/workflow-jobs).
