# cmux workspace handoff — implementation change details

Implementation-level change details for the `resume`/cmux workspace-handoff
defect. Companion to, and in three places a correction of,
[`CMUX_RESUME_HANDOFF_PLAN.md`](./CMUX_RESUME_HANDOFF_PLAN.md).

**Status of the plan's blocking gate: discharged.** The Sol plan deferred the
decisive question — it never executed `surface.report_pwd` — and instructed the
implementation worker to stop as *unsupported* unless a live smoke first proved
the semantics. That smoke has now been run against live cmux. The method is
verified supported, and the verbatim transcript is in
[§2](#2-live-evidence-the-discharged-gate). The implementation worker should
proceed directly and does **not** need to re-litigate it.

Environment for every command in this document:
`cmux 0.64.22 (102) [ddd4a01bc]`, macOS. If your cmux differs materially, re-run
[§2.6](#26-reproducing-the-smoke) before trusting the parsing details.

---

## 1. The defect, precisely

`resume` replaces itself with the selected native agent:

```rust
// src/launch.rs:195
#[cfg(unix)]
pub fn exec(spec: &ResumeSpec) -> io::Error {
    use std::os::unix::process::CommandExt;
    let mut command = Command::new(&spec.program);
    command.args(&spec.argv).current_dir(&spec.cwd);
    for (key, value) in &spec.env {
        command.env(key, value);
    }
    command.exec()
}
```

The replacement process's cwd becomes the Session Workspace. cmux workspace
metadata, however, still reports the directory the user launched `resume` from —
cmux learns a terminal's directory from the shell, and the shell never got a
chance to report, because `exec` replaced it.

The strict cmux-pi-orchestration binding check compares workspace metadata
against process cwd, finds them in disagreement, and blocks worker creation.

The fix repairs the state the validation observes. It does not weaken the
validation.

### 1.1 Scope

This is **shared launch behavior**, not an Agent Integration defect:

- `src/session.rs:85` — `ResumeSpec` already carries the authoritative target
  `cwd` beside `program`, `argv`, and narrow integration `env` overrides.
- `src/integration/{claude,codex,pi,omp,opencode}/resume.rs` each already set
  `ResumeSpec.cwd` from that integration's recorded Workspace.
- `src/app.rs:457` — `resume_selected` performs support checks,
  `launch::revalidate` (`:471`), mandatory confirmation, then `launch::exec`
  (`:489`).

cmux provenance is **process context**, not Session or integration launch state.
Therefore `ResumeSpec` and every `src/integration/*/resume.rs` stay unchanged.

---

## 2. Live evidence: the discharged gate

Read-only probes and one disposable-workspace mutation smoke. All disposable
resources were removed and the full workspace list re-diffed to zero drift.

### 2.1 The method exists and is advertised

```console
$ cmux capabilities --json     # → protocol cmux-socket, version 2, access_mode cmuxOnly
                               # → 303 methods, including:
system.identify
workspace.list
surface.report_pwd
```

`cmux rpc <method> [json-params]` is the documented raw v2 entry point
(confirmed in `cmux --help` and the published CLI contract).

### 2.2 Parameter schema, established by invalid-parameter probes

```console
$ cmux rpc surface.report_pwd '{}'
Error: invalid_params: Missing or invalid workspace_id
$ cmux rpc surface.report_pwd '{"workspace_id":"DA00…"}'
Error: invalid_params: Missing path
$ cmux rpc surface.report_pwd '{"workspace_id":"DA00…","surface_id":"0A68…"}'
Error: invalid_params: Missing path
$ cmux rpc surface.report_pwd '{"workspace_id":"DA00…","surface_id":"0A68…","path":7}'
Error: invalid_params: Missing path
$ cmux rpc surface.report_pwd '{"workspace_id":"DA00…","surface_id":7,"path":"/tmp"}'
Error: invalid_params: Missing or invalid surface_id
$ cmux rpc surface.report_pwd '{"workspace_id":"nope","surface_id":"0A68…","path":"/tmp"}'
Error: invalid_params: Missing or invalid workspace_id
```

Request shape: JSON object with string `workspace_id`, string `surface_id`,
string `path`.

Two refinements beyond the Sol plan:

- **`surface_id` is optional.** A workspace-only call succeeds and cmux selects a
  surface itself, returning which one it chose. We still send it explicitly so
  the mutation is fully addressed and never depends on cmux's choice.
- **Workspace UUID matching is case-insensitive.** A lowercased UUID was
  accepted. Do not rely on this; compare our own IDs case-insensitively so a
  differently-cased env var cannot cause a spurious mismatch.

### 2.3 The mutation smoke — the decisive evidence

A disposable workspace was created **unfocused** at `/tmp/cmux-smoke-a`, so the
smoke could never disturb the planning workspace:

```console
$ cmux workspace create --name "cmux-smoke-disposable" --cwd /tmp/cmux-smoke-a --focus false
OK workspace:120
   → id 2C593285-E605-4432-8120-1703075C4710, current_directory /tmp/cmux-smoke-a,
     selected false
$ cmux list-pane-surfaces --json --id-format uuids --workspace 2C59…
   → surface 5383A762-25AA-44CD-B976-C867FA49C830
```

Full 13-workspace state captured, then exactly one RPC:

```console
$ cmux rpc surface.report_pwd \
    '{"workspace_id":"2C593285-E605-4432-8120-1703075C4710",
      "surface_id":"5383A762-25AA-44CD-B976-C867FA49C830",
      "path":"/tmp/cmux-smoke-b"}'
{
  "path" : "/tmp/cmux-smoke-b",
  "surface_id" : "5383A762-25AA-44CD-B976-C867FA49C830",
  "surface_ref" : "surface:405",
  "workspace_id" : "2C593285-E605-4432-8120-1703075C4710",
  "workspace_ref" : "workspace:120"
}
exit=0
```

Pre/post diff across every workspace:

```text
changed entries: 1
('2C593285-…', ['/tmp/cmux-smoke-a', False], ['/tmp/cmux-smoke-b', False])
```

Focus and selection, before and after:

```text
caller   DA004F86-…  0A686DE3-…      (unchanged)
focused  2880D2DE-…                  (unchanged)
selected_workspace_id 2880D2DE-…     (unchanged)
```

After closing the disposable workspace, a re-diff against the original snapshot
reported **0** other workspaces changed.

**Conclusion.** `surface.report_pwd` updates exactly the addressed workspace's
`current_directory`, leaves `selected` false, and changes no focus or selection
state. This is precisely the acceptance condition the Sol plan set. It is met.

### 2.4 Read-back is synchronous

Five alternating report→list rounds with **no sleep** between them:

```text
round1 sent=/tmp/cmux-smoke-a rc=0 readback=/tmp/cmux-smoke-a match=YES
round2 sent=/tmp/cmux-smoke-b rc=0 readback=/tmp/cmux-smoke-b match=YES
round3 sent=/tmp/cmux-smoke-a rc=0 readback=/tmp/cmux-smoke-a match=YES
round4 sent=/tmp/cmux-smoke-b rc=0 readback=/tmp/cmux-smoke-b match=YES
round5 sent=/tmp/cmux-smoke-a rc=0 readback=/tmp/cmux-smoke-a match=YES
```

5/5. No polling, retry, or settle delay is needed or permitted.

### 2.5 cmux stores `path` verbatim — no normalization, no existence check

| sent | stored |
|---|---|
| `/tmp/cmux-smoke-b/` | `/tmp/cmux-smoke-b/` — trailing slash preserved |
| `/tmp/cmux-smoke-a/../cmux-smoke-b` | stored unresolved, verbatim |
| `relative/path` | stored as-is; relative paths accepted |
| `/tmp/definitely-does-not-exist-xyz123` | stored, **exit 0** — no existence check |
| `""` | rejected: `invalid_params: Missing path` |

This has a direct, load-bearing consequence for step 5 of the control flow:
**read-back must be exact byte equality against the string we sent.** Do not
canonicalize the read-back value. Canonicalizing it would mask a genuine
mismatch, and canonicalization of a path cmux merely echoed proves nothing.

The pre-state check in step 3 is the opposite case and *does* require
canonicalization — see [§3.1](#31-why-the-pre-state-check-must-canonicalize).

### 2.6 Reproducing the smoke

If your cmux version differs materially from `0.64.22 (102)`, re-run:

1. `cmux capabilities --json` — confirm `surface.report_pwd` is advertised.
2. Create a disposable workspace with `--focus false` at a temp directory A;
   record its id, its surface id, and the full `cmux workspace list --json
   --id-format uuids` output.
3. Issue exactly one `cmux rpc surface.report_pwd` with explicit
   `workspace_id`/`surface_id` and `path` = temp directory B.
4. Re-list; require **exactly one** changed entry, the addressed one.
5. Re-`identify`; require caller, focused, and `selected_workspace_id` unchanged.
6. `cmux workspace close` the disposable workspace; re-diff to zero drift.

If focus or selection changes, or another workspace changes, or read-back does
not expose B, **mark the API unsupported and stop.** Never substitute
`select-workspace`, `focus-pane`, or `focus-panel` as a workaround.

---

## 3. Corrections to the plan

Three points where live evidence contradicts `CMUX_RESUME_HANDOFF_PLAN.md`. The
plan's control flow and safety posture are otherwise adopted unchanged.

### 3.1 Why the pre-state check must canonicalize

The plan says to canonicalize both sides of the pre-state comparison. That is
correct, and the reason is concrete. Verified with a compiled Rust probe:

```text
cd /tmp/cwdtest/link                    # symlink → /tmp/cwdtest/real

std::env::current_dir()   = "/private/tmp/cwdtest/real"   # physical
$PWD as seen by the shell = "/tmp/cwdtest/link"           # logical
```

cmux holds the **shell's logical `$PWD`**, symlinks intact. Rust's
`current_dir()` returns the **physical** path. Comparing raw strings would
spuriously fail for every user working inside a symlinked tree — a very common
setup with worktrees and `/tmp` on macOS, where `/tmp` is itself a symlink to
`/private/tmp`.

### 3.2 Correction: send the canonicalized path, not `ResumeSpec.cwd` verbatim

The plan says to send `T` as the OS path from `ResumeSpec.cwd` unmodified. Live
evidence shows that is wrong in the symlink case. Same probe:

```text
Command::current_dir("/tmp/cwdtest/link") → child getcwd = /private/tmp/cwdtest/real
```

`Command::current_dir` resolves symlinks: the agent's own `getcwd()` will be the
**physical** path. The orchestration check compares cmux metadata against process
cwd. Sending a symlinked spelling would therefore leave metadata and process cwd
disagreeing — reintroducing precisely the defect being fixed, in a subtler form.

**Send `canonicalize(ResumeSpec.cwd)`.** Read-back then compares byte-for-byte
against that same canonical string. Because `revalidate` (`src/launch.rs:119`)
has already confirmed the Workspace exists immediately beforehand,
canonicalization is expected to succeed; failure is fatal and authorizes no
mutation.

Minor, accepted consequence: the cmux UI displays the resolved physical path
rather than the user's symlinked spelling. Correctness of the binding check wins
over cosmetic path spelling.

### 3.3 Correction: what `identify` actually guarantees

The plan treats `identify`'s `.caller` as independent proof of caller identity,
and says a mismatch means "the env-identified caller workspace is still bound to
this process." That overstates it. `.caller` is **derived from the same
environment variables being validated**:

```console
$ CMUX_WORKSPACE_ID=<foreign> CMUX_SURFACE_ID=<foreign> cmux identify --json --id-format uuids
   → .caller reports the FOREIGN pair
$ env -u CMUX_WORKSPACE_ID -u CMUX_SURFACE_ID cmux identify --json --id-format uuids
   → .caller: null
```

So `identify` cannot detect a spoofed or stale environment. Feeding it the env
values and checking they come back is partly circular.

It is still worth calling, for two things it genuinely establishes:

1. **Pair existence and consistency.** Real-but-mismatched IDs are rejected:
   `--workspace DA004F86… --surface 124375E7…` (both real, wrong pairing) →
   `.caller: null`. A single ID alone also resolves, so only the *pair* check is
   meaningful.
2. **`app_cli_path`** — the server-reported CLI path, so verification and
   mutation use the same cmux binary rather than re-resolving `PATH`.

**The load-bearing authorization remains the pre-state check**: exactly one
workspace with `id == W`, whose canonicalized `current_directory` equals the
canonicalized `std::env::current_dir()`. That is what actually proves this
process is running where cmux thinks that workspace is. The plan identified this
guard correctly; this correction only reassigns which guard carries the weight.

Defence in depth: `report_pwd` independently rejects an inconsistent pair with
`not_found: Surface not found`, so a spoofed pair cannot silently mutate a
foreign workspace even if it somehow passed our checks.

### 3.4 The prompt-overwrite lifetime, and why it is acceptable

Not addressed in the plan. cmux also learns a terminal's directory from the shell
at each prompt (OSC 7). Verified:

```console
# after a successful report to B, send any command + Enter:
after benign cmd + prompt: '/tmp/cmux-smoke-a'    # reverted
```

The reported value is overwritten at the next shell prompt, because the shell
re-reports its own real cwd.

**This does not break the design, and the reason matters.** During a
long-running **foreground** process the reported value persists — verified across
a 25-second `sleep`, where the value stayed at B for the entire duration:

```text
1b during command:   dir= '/tmp/cmux-smoke-a'
2 report T while command runs → rpc exit=0
                     dir= '/tmp/cmux-smoke-b'
3 still during command:
                     dir= '/tmp/cmux-smoke-b'
```

`exec` replaces `resume` with the agent, which holds the terminal for its entire
lifetime. **No prompt fires until the agent exits.** The corrected value
therefore survives exactly as long as the agent that needs it — which is the
whole window the orchestration binding check cares about.

When the agent finally exits and the shell prompts again, the shell re-reports
its own real cwd. That is correct, self-healing, and requires no action from
`resume`. It independently confirms the plan's "never roll back" rule: a rollback
would be a second unverified mutation racing a shell that is about to correct the
value anyway.

Document this as accepted, self-limiting behavior.

---

## 4. Exact control flow

Ordered. Every step names its failure variant. All failures occur **before**
`exec`.

### Step 0 — preserve existing gates, unchanged

Selection, terminal restoration, support checks, `launch::revalidate`, and user
confirmation stay exactly where they are in `resume_selected`. A declined
confirmation remains a no-op returning `EXIT_OK`: it must not contact or mutate
cmux. The handoff is inserted strictly between confirmation and
`launch::exec(spec)` — i.e. immediately before `src/app.rs:489`.

### Step 1 — classify the environment

Read `CMUX_WORKSPACE_ID` and `CMUX_SURFACE_ID` as `OsString`. Never modify or
clear them.

| environment | result |
|---|---|
| both absent | `NotCmux` → return `Ok(())`, spawn nothing |
| both present, both non-empty | proceed to verified handoff |
| exactly one present | `IncompleteEnv` |
| either present but empty | `IncompleteEnv` |
| either present but not valid UTF-8 | `IncompleteEnv` |

Both-absent is the **only** no-op path. Partial or empty provenance claims a
malformed cmux context and must fail closed rather than silently conceal it.

The UTF-8 requirement is not a limitation in practice: cmux IDs are UUIDs, and
the env-stripped probe in §3.3 confirms a non-cmux process simply has neither
variable set.

### Step 2 — capture origin

`std::env::current_dir()` → `origin`. Failure is `OriginUnavailable`.
Canonicalize it; failure is `OriginUnavailable`.

### Step 3 — verify the caller pair, and learn the CLI path

Invoke the cmux CLI **directly, never through a shell**:

```text
cmux identify --json --id-format uuids --workspace <W> --surface <S>
```

Pass both IDs explicitly so inherited defaults cannot drift.

For this first call only, resolve `cmux` via `PATH` using the existing
`launch::command_available` helper (`src/launch.rs:97`); if it is not found,
`CliUnavailable`.

Require, in order:

| condition | failure variant |
|---|---|
| process spawns | `IdentifySpawn(io::Error)` |
| exit status 0 | `IdentifyStatus { status, stderr }` |
| stdout parses as JSON | `IdentifyJson` |
| `.caller` is a non-null object | `CallerMismatch` |
| `.caller.workspace_id` equals `W`, ASCII-case-insensitively | `CallerMismatch` |
| `.caller.surface_id` equals `S`, ASCII-case-insensitively | `CallerMismatch` |
| `.app_cli_path` is a non-empty string | `CliPathUnavailable` |
| that path is an existing executable file | `CliPathUnavailable` |

Ignore `.focused` entirely — it is UI state, never authorization. Reuse
`app_cli_path` for **both** remaining calls.

Bound stderr captured for diagnostics (see [§6](#6-failure-semantics)).

### Step 4 — verify pre-state (the load-bearing guard)

```text
<app_cli_path> workspace list --json --id-format uuids
```

Canonical `workspace list`, not the legacy `list-workspaces` alias. (The alias
does work and prints its deprecation notice to *stderr*, leaving stdout JSON
clean — but there is no reason to depend on that.) Do **not** set `CMUX_QUIET`.

| condition | failure variant |
|---|---|
| process spawns | `ListSpawn(io::Error)` |
| exit status 0 | `ListStatus { status, stderr }` |
| stdout parses as JSON | `ListJson` |
| `.workspaces` is an array | `ListJson` |
| **exactly one** element with `id == W` (case-insensitive) | `WorkspaceNotUnique { count }` |
| that element's `current_directory` is a string | `ListJson` |
| it canonicalizes successfully | `PreStateMismatch { expected, actual }` |
| canonical form equals canonical `origin` | `PreStateMismatch { expected, actual }` |

Zero matches and duplicates are both fatal. This step authorizes the mutation:
it proves the env-named workspace is the one this process is actually running in.

### Step 5 — compute the target

`target = canonicalize(ResumeSpec.cwd)`; failure is `TargetUnavailable`.

Then require `target` to be valid UTF-8 → `NonUtf8Target`. JSON strings require
Unicode, and lossy conversion would silently mutate cmux to a *different*
directory than the agent will actually run in. Reject instead.

This restriction applies **only** to the verified-cmux path. The existing
non-cmux Unix launch continues to preserve non-UTF-8 `ResumeSpec` paths exactly
as today, as pinned by `resume_spec_preserves_non_utf8_path_and_argv`
(`src/session.rs`).

### Step 6 — the single addressed mutation

Exactly one call, to the verified `app_cli_path`, with three argv elements:

```text
<app_cli_path> rpc surface.report_pwd {"workspace_id":"<W>","surface_id":"<S>","path":"<target>"}
```

Build the JSON with `serde_json` (already a dependency — `Cargo.toml`). Never
interpolate or hand-quote it. Never use `sh -c`.

| condition | failure variant |
|---|---|
| process spawns | `ReportSpawn(io::Error)` |
| exit status 0 | `ReportStatus { status, stderr }` |

The success response body is **not** parsed. It echoes the request, and §2.5
shows cmux accepts paths it has not validated, so the response is not evidence
that the orchestration-visible field reached the required value. Step 7 is.

No retry. No fallback to another method. No focus or selection command.

### Step 7 — read back before `exec`

Repeat the step 4 command through the same `app_cli_path`.

| condition | failure variant |
|---|---|
| process spawns | `ReadbackSpawn(io::Error)` |
| exit status 0 | `ReadbackStatus { status, stderr }` |
| stdout parses as JSON, `.workspaces` array | `ReadbackJson` |
| exactly one element with `id == W` | `WorkspaceNotUnique { count }` |
| `current_directory` **byte-equals** `target` | `ReadbackMismatch { expected, actual }` |

Exact string equality, per §2.5 — **do not canonicalize the read-back value**.

§2.4 proves this is synchronous, so a single read suffices.

### Step 8 — process replacement, unchanged

Only on `Ok(())` does `resume_selected` proceed to the existing
`launch::exec(spec)` at `src/app.rs:489`. `exec` still sets the replacement
process cwd itself via `Command::current_dir(&spec.cwd)`. cmux metadata is
**never** substituted for that.

No async work, no thread, no child wrapper around the native agent. A successful
`exec` never returns, so the agent retains terminal ownership, signals, and
eventual exit status.

---

## 5. API boundary

### `src/launch.rs`

```rust
/// Outcome of the cmux workspace handoff. Crate-visible; no public CLI flag.
#[derive(Debug)]
pub(crate) enum CmuxHandoffError {
    IncompleteEnv(&'static str),
    OriginUnavailable(io::Error),
    CliUnavailable,
    CliPathUnavailable,
    IdentifySpawn(io::Error),
    IdentifyStatus { status: ExitStatus, stderr: String },
    IdentifyJson(&'static str),
    CallerMismatch,
    ListSpawn(io::Error),
    ListStatus { status: ExitStatus, stderr: String },
    ListJson(&'static str),
    WorkspaceNotUnique { count: usize },
    PreStateMismatch { expected: PathBuf, actual: String },
    TargetUnavailable(io::Error),
    NonUtf8Target,
    ReportSpawn(io::Error),
    ReportStatus { status: ExitStatus, stderr: String },
    ReadbackSpawn(io::Error),
    ReadbackStatus { status: ExitStatus, stderr: String },
    ReadbackJson(&'static str),
    ReadbackMismatch { expected: String, actual: String },
}

impl std::fmt::Display for CmuxHandoffError { /* one specific line per variant */ }

/// Production entry point. Reads the process environment and current
/// directory, then delegates to the pure core.
#[cfg(unix)]
pub(crate) fn handoff_cmux_workspace(spec: &ResumeSpec) -> Result<(), CmuxHandoffError>;
```

The pure core — this is what the tests drive, and it is why almost no test needs
to mutate process environment:

```rust
/// Every input explicit. No `std::env` access, no `PATH` resolution.
#[cfg(unix)]
fn handoff_with(
    workspace_env: Option<&OsStr>,
    surface_env: Option<&OsStr>,
    origin: &Path,
    target: &Path,
    runner: &dyn CmuxRunner,
) -> Result<(), CmuxHandoffError>;

/// Seam making the fixed three-call sequence observable without a live socket.
trait CmuxRunner {
    /// `program` is `None` for the initial PATH-resolved `identify`,
    /// `Some(app_cli_path)` for every later call.
    fn run(&self, program: Option<&Path>, args: &[&OsStr]) -> io::Result<Output>;
}
```

Keep `CmuxRunner` private to `launch.rs`. It exists to make one fixed sequence
testable — it must not grow into a general process-execution framework.

`exec` remains untouched as the real `CommandExt::exec` boundary. cmux mutation
is never folded into an integration constructor.

### `src/app.rs`

One insertion in `resume_selected`, between the confirmation block and
`launch::exec(spec)` at `:489`:

```rust
    if let Err(error) = launch::handoff_cmux_workspace(spec) {
        eprintln!("resume: cmux workspace handoff failed: {error}");
        return EXIT_ERROR;
    }
    let error = launch::exec(spec);
```

If the implementation prefers a single private `launch::handoff_and_exec` to make
ordering testable, that is acceptable provided it stays private and synchronous.
Do not add a generic abstraction merely to rename two calls.

### Files that must not change

- `src/session.rs` — `ResumeSpec` already supplies the target.
- `src/integration/*/resume.rs` and their launch-contract tests.
- `src/errors.rs` — see [§6](#6-failure-semantics).
- `Cargo.toml` / `Cargo.lock` — `serde_json` and `std::process` suffice.
- cmux-pi-orchestration validation itself.

---

## 6. Failure semantics

Every failure prints one line to stderr and returns the existing `EXIT_ERROR`
(`src/app.rs:32`):

```text
resume: cmux workspace handoff failed: <specific reason>
```

**No new `src/errors.rs` catalog entry.** This matches the plan and the
surrounding `resume: …` diagnostics already in `resume_selected`. Adding a code
would expand the catalog, break the pinned `catalog_has_seven_entries` test, and
change `--help` and man-page output — disproportionate to the defect.

Each variant in §5 renders a distinguishable reason, so a user can tell an
incomplete environment from a caller mismatch from a read-back mismatch.

Rules:

- **No retry, no fallback, no best-effort suppression.** A failure after the
  report may mean cmux already holds the target; report it and stop. Concealing
  it by proceeding to `exec` would violate the required verified transition.
- **No rollback.** If cmux update and read-back succeed but `exec` then fails
  (e.g. the agent binary disappears in the final race), return the existing
  launch error and leave cmux at the target. Rollback would be a second
  unverified mutation, would race the shell's own reporting, and would conceal
  the actual `exec` failure. §3.4 shows the next shell prompt corrects the value
  naturally. Note this residual partial transition in the diagnostic.
- **Diagnostics hygiene.** Capture subprocess stdout for JSON; surface only
  bounded, path-safe stderr and exit status. Truncate captured stderr (1 KiB is
  ample for the observed one-line `Error: …` messages). Never print Session
  transcript content, socket passwords, or environment values.
- **No persistent state.** Do not cache or write caller IDs, socket paths,
  previous directories, or rollback state anywhere.

### Environment semantics

- Do not add cmux fields to `ResumeSpec.env`; integration overrides stay narrow.
- Do not clear or rewrite `CMUX_WORKSPACE_ID`, `CMUX_SURFACE_ID`,
  `CMUX_SOCKET_PATH`, or `CMUX_SOCKET_PASSWORD`. Subprocesses and the replacement
  agent inherit the launcher environment normally. (The published cmux contract
  confirms these are protected variables cmux itself manages.)
- Always pass W and S explicitly; inherited defaults are context, not
  authorization.

---

## 7. Platform scope

- Gate the handoff and `exec` under the existing `#[cfg(unix)]` behavior. macOS
  and Linux remain the supported platforms.
- Behavior is **runtime**-gated: a Unix build outside cmux takes the both-absent
  no-op path and never requires cmux to be installed.
- Inside a cmux-marked environment, a working, compatible cmux CLI is required;
  failure is explicit and fatal.
- No Windows behavior. Windows remains unsupported — no native process
  replacement.
- Deterministic fake-process tests run on macOS and Linux CI. The live smoke of
  §2.6 is macOS-specific until a verified Linux cmux runtime exists; Linux still
  covers the non-cmux path and the full fake protocol sequence.

---

## 8. Tests

Fixture strategy, mirroring the existing in-tree pattern:

- A **fake `cmux` shell script** written to a `tempfile::tempdir()`, `chmod`
  0o755, emitting canned JSON on stdout and appending each invocation's argv to a
  **capture file**. This mirrors `fake_pi` (`src/integration/pi/tests/resume.rs:225`)
  and `run_resume_spec_capturing` (`:247`). It exercises real spawning, real
  argv, real exit codes, and the real stdout/stderr split — with no live cmux and
  no socket.
- Tests drive `handoff_with`, passing env values, `origin`, `target`, and the
  runner **explicitly**. Because `std::env::set_var` is `unsafe` in edition 2024
  (`Cargo.toml`), this keeps essentially every test free of process-global env
  mutation. At most one thin test covers the `std::env` wrapper.
- The capture file makes **call order and count** assertable, which is what
  proves "no mutation occurred" and "exactly one report was sent".

| # | test | asserts |
|---|---|---|
| 1 | `no_cmux_env_is_noop` | both IDs absent → `Ok(())`, capture file empty (zero cmux invocations) |
| 2 | `partial_cmux_env_fails_without_mutation` | table: only-W, only-S, empty-W, empty-S → `IncompleteEnv`; capture file empty; no agent exec |
| 3 | `caller_id_mismatch_fails_without_mutation` | fake identify differs in W or S — including a case where `.focused` *does* match — → `CallerMismatch`; capture shows identify only, no list, no report |
| 4 | `workspace_directory_mismatch_fails_without_mutation` | IDs match, W's `current_directory` ≠ origin → `PreStateMismatch`; capture shows identify + list only |
| 5 | `workspace_match_requires_exactly_one_id` | zero matches and duplicate matches both → `WorkspaceNotUnique`; no report |
| 6 | `verified_caller_reports_target_before_exec` | capture is exactly: identify, list, one `surface.report_pwd` carrying W/S/target, list. Then the fake agent runs and records cwd == target, native argv, inherited cmux IDs, integration env overrides |
| 7 | `report_pwd_failure_prevents_exec` | fake returns nonzero for the RPC → `ReportStatus`; agent marker file absent; capture shows exactly one report (no retry) |
| 8 | `post_report_readback_mismatch_prevents_exec` | RPC exits 0 but post-list still shows origin → `ReadbackMismatch`; agent marker absent; one report only |
| 9 | `malformed_cmux_json_fails_closed` | table: invalid JSON, `.caller` null, missing `app_cli_path`, non-string `current_directory`, missing `.workspaces` → correct variant each, no unauthorized mutation |
| 10 | `verified_cli_path_is_reused` | after identify, list and RPC invoke the `app_cli_path` from the identify response, not a second PATH lookup — proven by pointing `app_cli_path` at a *second* fake script with its own capture file |
| 11 | `exec_failure_after_handoff_is_not_rolled_back` | read-back reaches target, native launch fails → capture contains **no** second report back to origin; the existing launch error is returned |
| 12 | `non_utf8_cmux_target_fails_without_lossy_mutation` (`cfg(unix)`) | verified cmux context with non-UTF-8 target → `NonUtf8Target` before any RPC; capture shows no report |
| 13 | `symlinked_workspace_reports_canonical_path` | origin and target reached via symlink: pre-state check passes despite logical/physical divergence (§3.1), and the reported path is the canonical physical form (§3.2) |
| 14 | `readback_requires_exact_string_match` | cmux echoes a trailing-slash or `..`-containing variant of the target → `ReadbackMismatch`, proving read-back is not canonicalized (§2.5) |

Tests 13 and 14 are additions beyond the plan, covering the two corrections in
§3.1–3.2 and the verbatim-storage finding in §2.5.

While implementing, run only the focused filter — e.g.
`cargo test --locked launch::tests::` plus the smallest fake-agent integration
case. Project-wide suites (`make ci`) belong to final CI, not this lane.

---

## 9. Documentation

`docs/product-design.md` §6 "Resume safety and process handoff" (line 355) lists
the pre-handoff sequence. Insert a step between the current 5 and 6 — i.e.
between "Apply only integration-required environment overrides" and "Call Unix
`exec`" — and add a short subsection:

- When, and only when, both cmux caller IDs are present and verified, `resume`
  synchronously updates and reads back **its own** cmux workspace directory
  before `exec`, so cmux metadata matches the Workspace the agent will run in.
- Ordinary non-cmux invocation is completely unchanged and never requires cmux.
- Incomplete or mismatched cmux provenance, or any mutation/read-back failure,
  exits 1 **before** `exec`.
- No workspace, pane, surface, or tab is created, selected, focused, moved, or
  closed.
- If `exec` fails after a confirmed update, cmux remains at the selected
  Workspace and the launch error is surfaced; the next shell prompt re-reports
  the shell's own directory naturally (§3.4).

This is a public behavior change only inside cmux, so §6 is the correct and only
required documentation site. The exit-status list in §6 already covers this: "1:
… final validation failed" — no change needed there.

---

## 10. Checklist

- [ ] Insert the Unix handoff immediately before `exec`, after revalidation and confirmation (`src/app.rs:489`).
- [ ] Make both-absent IDs the only no-op path.
- [ ] Reject incomplete, empty, or non-UTF-8 cmux provenance.
- [ ] Verify the `.caller` W/S **pair**; never authorize from `.focused` or `selected`.
- [ ] Require exactly one W, with canonical `current_directory` == canonical `current_dir()`.
- [ ] Use the server-returned `app_cli_path` for the list and RPC calls; direct argv, never a shell.
- [ ] Send **canonicalized** target; reject non-UTF-8 before the RPC.
- [ ] Issue exactly one `surface.report_pwd` with explicit W/S/path.
- [ ] Read back exactly one W and require **byte-exact** equality with the sent string.
- [ ] Leave `Command::current_dir(T).exec()` and integration env handling unchanged.
- [ ] Expose every failure; no retry, fallback, focus/select, rollback, or persistent state.
- [ ] Add tests 1–14; keep `src/errors.rs` untouched.
- [ ] Update `docs/product-design.md` §6.
- [ ] Re-run the §2.6 smoke only if the local cmux version differs materially.

---

## Appendix: verified cmux command reference

Everything here was executed directly. Nothing is inferred.

### Commands used by the implementation

```text
cmux identify --json --id-format uuids --workspace <W> --surface <S>
<app_cli_path> workspace list --json --id-format uuids
<app_cli_path> rpc surface.report_pwd '{"workspace_id":"<W>","surface_id":"<S>","path":"<T>"}'
```

### Response fields relied upon

| command | field | type |
|---|---|---|
| `identify` | `.caller.workspace_id` | string UUID, or `.caller` null |
| `identify` | `.caller.surface_id` | string UUID |
| `identify` | `.app_cli_path` | absolute path string |
| `workspace list` | `.workspaces[]` | array |
| `workspace list` | `.workspaces[].id` | string UUID |
| `workspace list` | `.workspaces[].current_directory` | string |

Observed full workspace object keys: `current_directory`, `custom_color`,
`custom_title`, `description`, `has_custom_title`, `id`, `index`,
`latest_conversation_message`, `latest_submitted_at`,
`latest_submitted_message`, `listening_ports`, `pinned`, `remote`, `selected`,
`title`. Only `id` and `current_directory` are used.

### Error taxonomy — stderr, exit 1; success JSON on stdout, exit 0

| condition | message |
|---|---|
| missing/invalid `workspace_id` | `invalid_params: Missing or invalid workspace_id` |
| missing, empty, or non-string `path` | `invalid_params: Missing path` |
| non-string `surface_id` | `invalid_params: Missing or invalid surface_id` |
| valid UUID, no such workspace | `not_found: Workspace not found` |
| surface not in that workspace | `not_found: Surface not found` |
| unknown method | `method_not_found: Unknown method` |
| socket absent | `Socket not found at <path>` |
| invalid handle to `identify` | `Invalid surface handle: <v> (expected UUID, ref like surface:1, or index)` |

Success output goes to **stdout**; all errors to **stderr**. Exit is 0 on success
and 1 on every failure above — including `method_not_found`, which makes an
incompatible cmux version fail closed rather than silently no-op.

### Explicitly rejected alternatives

| command | why not |
|---|---|
| `select-workspace` / `workspace select` | changes selection — forbidden and unnecessary |
| `focus-pane`, `focus-panel`, `focus-window` | changes focus — forbidden |
| `workspace-action` | supports rename/pin/color/move/close; **no** current-directory action |
| `workspace create --cwd` | `--cwd` exists only at creation; creating a workspace is forbidden |
| `list-workspaces` (legacy alias) | works, but prints a deprecation notice; canonical `workspace list` preferred |
| `mobile.workspace.list` | advertised, but no reason to prefer it over `workspace.list` |

No documented top-level CLI command updates an existing workspace's directory.
`surface.report_pwd` via `rpc` is the only evidenced mechanism — and, per §2.3,
a verified one.
