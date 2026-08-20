# cmux workspace handoff plan for `resume`

## Decision

Add one synchronous, Unix-only pre-`exec` handoff in the shared launch path. When, and only when, both cmux caller IDs are present and independently verified against live cmux caller state, report the selected Session Workspace to that exact caller surface/workspace, read the workspace back, and require its `current_directory` to equal the selected Workspace before calling the existing native `exec`.

The intended state transition is:

```text
verified caller workspace W, verified caller surface S
W.current_directory == process current_dir == launch directory
ResumeSpec.cwd == selected, revalidated Session Workspace T

surface.report_pwd(workspace_id=W, surface_id=S, path=T)

then require:
W.current_directory == T

then and only then:
Command::new(spec.program)
    .args(spec.argv)
    .current_dir(T)
    .env(spec.env)
    .exec()
```

This repairs cmux state instead of weakening the mandatory cmux-pi-orchestration binding check. It does not create, select, focus, move, or close any workspace, pane, surface, or tab. It adds no dependency, cache, state file, daemon, thread, retry, or child wrapper around the native agent.

One implementation condition remains intentionally explicit: this investigation did not execute `surface.report_pwd`, because its assignment was read-only. The implementation worker must first run the narrow live smoke test in [Live acceptance smoke](#live-acceptance-smoke). If that test does not prove that the advertised method updates only the addressed caller workspace's `current_directory` without changing selection/focus, stop as unsupported; do not substitute an inferred cmux command.

## Evidence and confidence boundary

Evidence was collected from cmux `0.64.22 (102) [ddd4a01bc]` on macOS. Commands below were read-only or invalid-parameter probes.

### Verified from live CLI help/runtime

1. `cmux --help` documents:
   - `CMUX_WORKSPACE_ID`: auto-set in cmux terminals and the default workspace for commands.
   - `CMUX_SURFACE_ID`: auto-set in cmux terminals and the default surface.
   - `cmux identify [--workspace ...] [--surface ...]`.
   - canonical `cmux workspace list --json` (the observed legacy alias explicitly directs callers to it).
   - raw `cmux rpc <method> [json-params]`, where params are an optional JSON object.
2. In this process, the env values were:

   ```text
   CMUX_WORKSPACE_ID=DA004F86-E71B-44F8-9C53-25C747E02E9C
   CMUX_SURFACE_ID=D698D7D6-B11B-4345-B772-FCCB6ED0C1ED
   PWD=/Users/luwei.will/ai/resume
   ```
3. `cmux identify --json --id-format uuids` returned these exact fields:

   ```json
   {
     "app_cli_path": "/Applications/cmux.app/Contents/Resources/bin/cmux",
     "caller": {
       "workspace_id": "DA004F86-E71B-44F8-9C53-25C747E02E9C",
       "surface_id": "D698D7D6-B11B-4345-B772-FCCB6ED0C1ED"
     },
     "focused": {
       "workspace_id": "2BDC2931-C681-4D6A-8639-4F6903A623A5",
       "surface_id": "B15752C6-B800-4C18-AD73-B9AA11FA0F74"
     }
   }
   ```

   Therefore caller identity is `.caller`, not `.focused`, and must not be inferred from selected/focused UI state. The observed caller IDs matched both env IDs even while another workspace was focused.
4. `cmux workspace list --json --id-format uuids` returned workspace objects containing `id`, `current_directory`, and `selected`. Exactly one object had the caller workspace ID and:

   ```json
   {
     "id": "DA004F86-E71B-44F8-9C53-25C747E02E9C",
     "current_directory": "/Users/luwei.will/ai/resume",
     "selected": false
   }
   ```

   Its directory matched this process's actual current directory. `selected: false` again shows that selection is neither caller proof nor a prerequisite for an addressed update.
5. `cmux capabilities --json` advertised all of:
   - method `system.identify`;
   - method `workspace.list`;
   - method `surface.report_pwd`.
6. Raw invalid-parameter probes established the required `surface.report_pwd` field names and types without performing a mutation:
   - `{}` -> `Missing or invalid workspace_id`;
   - valid `workspace_id` only -> `Missing path`;
   - valid `workspace_id` and `surface_id` but no path -> `Missing path`;
   - numeric `path` -> `Missing path`;
   - numeric `surface_id` -> `Missing or invalid surface_id`;
   - non-UUID `workspace_id` -> `Missing or invalid workspace_id`.

   Thus the proposed request is the live-advertised raw v2 method with a JSON object containing string `workspace_id`, string `surface_id`, and string `path`.

### Inference that implementation must prove

The method name, capability advertisement, accepted parameter schema, and cmux's existing shell-reported `current_directory` make `surface.report_pwd` the only evidenced candidate for the handoff. This report does **not** claim to have observed a successful state update, its response shape, or its focus behavior. Those are acceptance conditions, not assumed facts.

No documented top-level CLI command updates an existing workspace directory. In particular, live help showed `--cwd` only on **workspace creation**, while `workspace-action` supports metadata/actions such as rename, pin, and move—not current-directory reporting. `select-workspace`, `focus-pane`, and `focus-panel` exist but are forbidden and unnecessary.

## Existing code boundary

The defect is shared launch behavior, not an Agent Integration defect:

- `src/session.rs::ResumeSpec` already carries the authoritative target `cwd` beside native program/argv and narrow integration env overrides.
- Every inspected constructor—`src/integration/{claude,codex,pi,omp,opencode}/resume.rs`—sets `ResumeSpec.cwd` from that integration's recorded Workspace. Existing focused launch-contract tests execute fake agents with `Command::current_dir(&spec.cwd)` and assert native argv/cwd/environment.
- `src/app.rs::resume_selected` currently performs support checks, final `launch::revalidate`, mandatory confirmation if needed, and then `launch::exec(spec)`.
- Unix `src/launch.rs::exec` applies argv, `current_dir(&spec.cwd)`, integration env overrides, and `CommandExt::exec`.

Therefore do not modify `ResumeSpec` or any `src/integration/*/resume.rs`. cmux provenance is process context, not Session/integration launch state.

## Exact control flow

### 1. Preserve existing safety gates

Keep selection, terminal restoration, support checks, `launch::revalidate`, and any user confirmation exactly where they are. A declined confirmation remains a no-op: it must not contact or mutate cmux.

### 2. Classify the environment once, immediately before handoff

Read `CMUX_WORKSPACE_ID` and `CMUX_SURFACE_ID` as OS strings (do not alter or clear them):

| Environment | Result |
|---|---|
| both absent | `NotCmux`; return success without spawning cmux |
| both present and non-empty | attempt verified cmux handoff |
| only one present, or either present but empty | error; do not invoke `surface.report_pwd`; do not exec |

The partial/empty case is not the explicit non-cmux path. It claims malformed cmux provenance and must fail closed rather than silently conceal it.

### 3. Verify the caller before mutation

For both-present provenance, synchronously:

1. Capture `std::env::current_dir()` as `origin`. Failure is fatal.
2. Invoke the live cmux CLI directly (no shell) for `identify --json --id-format uuids`, explicitly passing both `--workspace <env W>` and `--surface <env S>` so defaults cannot drift.
3. Require successful exit, valid JSON, `.caller.workspace_id == W`, and `.caller.surface_id == S`. A missing/invalid/mismatched caller is fatal and authorizes no mutation. Ignore `.focused` for authorization.
4. Use the returned non-empty absolute `app_cli_path` for the remaining calls in this handoff, so verification and mutation use the server-reported cmux CLI rather than re-resolving a potentially different `PATH` entry. If absent/invalid/unexecutable, fail before mutation. In tests, the fake identify response supplies its own fake executable path.
5. Invoke that exact CLI as `workspace list --json --id-format uuids`. Require successful exit and valid JSON.
6. Select by exact UUID equality. Require **exactly one** workspace object with `id == W`, a string `current_directory`, and that directory equal `origin` by filesystem path identity suitable for existing paths (canonicalize both; do not rely on lossy display or lexical aliases). Zero/duplicate matches, malformed fields, canonicalization failure, or mismatch is fatal and authorizes no mutation.

The directory precondition is load-bearing: it proves the env-identified caller workspace is still bound to this process's launch context. The fact that `ResumeSpec.cwd` was revalidated does not prove this separate caller binding.

### 4. Perform exactly one addressed report

After all guards pass, invoke the verified `app_cli_path` directly, once, with:

```text
rpc
surface.report_pwd
{"workspace_id":"W","surface_id":"S","path":"T"}
```

Construct the JSON with `serde_json`; never interpolate/quote it manually and never use `sh -c`. `T` is the OS path from the already-revalidated `ResumeSpec.cwd`. Because JSON strings require Unicode, reject a non-UTF-8 target path in the cmux path rather than lossy-converting it. This restriction applies only to verified cmux handoff; the existing non-cmux Unix launch continues to preserve non-UTF-8 `ResumeSpec` paths.

Require process spawn success and zero exit status. Any failure is fatal. Do not retry, fall back to another method, select/focus anything, or continue to native `exec`.

### 5. Read back before `exec`

Call `workspace list --json --id-format uuids` again through the same verified CLI. Require exactly one workspace with `id == W` and canonical `current_directory == canonical T`. A command, JSON, cardinality, field, or equality failure is fatal and prevents `exec`.

This read-back is required because a successful RPC transport response alone is not evidence that the orchestration-visible field reached the required value.

### 6. Preserve native process replacement

Only after successful read-back call the existing Unix `launch::exec(spec)`. It still sets the replacement process cwd itself; cmux metadata is not substituted for `Command::current_dir`.

There is no async work and no child-agent wrapper. A successful `exec` never returns, so the native agent retains terminal ownership, signals, and eventual status.

## Environment semantics

- Do not add cmux fields to `ResumeSpec.env`; integration env overrides remain narrow and unchanged.
- Do not clear or rewrite `CMUX_WORKSPACE_ID`, `CMUX_SURFACE_ID`, `CMUX_SOCKET_PATH`, or `CMUX_SOCKET_PASSWORD`. The cmux subprocesses and replacement agent inherit the launcher's environment exactly as normal, plus only existing integration overrides on the replacement command.
- Always pass explicit W/S arguments or JSON fields during verification/mutation; inherited defaults are context, not authorization.
- Do not set `CMUX_QUIET`; use canonical `workspace list`, which avoids the observed legacy-alias notice.
- Capture subprocess stdout for JSON and surface bounded, path-safe stderr/status diagnostics on failure. Do not print Session transcript data or credentials.
- Do not persist caller IDs, socket paths, old/new directories, or rollback state.

## Failure semantics

All verified-cmux failures occur before native `exec`, print a specific `resume: cmux workspace handoff failed: ...` diagnostic, and return existing `EXIT_ERROR` (1). Distinguish at least: incomplete env, CLI unavailable, identify command/status/JSON failure, caller mismatch, workspace-list failure, missing/duplicate workspace, pre-state cwd mismatch, non-UTF-8 target, report spawn/status failure, and read-back mismatch.

There is deliberately no retry or fallback. A failure after the report may mean cmux already holds T; report that failure and stop. Concealing it by exec would violate the required verified transition.

If cmux update and read-back succeed but `exec` later fails (for example, the agent disappears in the final race), the existing launch error is returned and cmux remains at T. Do **not** roll back: rollback would be a second unverified mutation, can race shell/agent reporting, and can conceal the actual exec failure. State this residual partial transition in the diagnostic/documentation. The next shell prompt's normal cmux integration may report its own cwd; `resume` must not depend on that as fallback.

## Unix/macOS/Linux scope

- Gate production handoff and `exec` under existing `#[cfg(unix)]` behavior. macOS and Linux remain the supported platforms.
- The protocol behavior is runtime-gated: a Unix build outside cmux takes the exact no-op path and does not require `cmux` to be installed.
- A cmux-marked environment requires a working compatible cmux CLI and advertised method; failure is explicit.
- Do not add Windows behavior. Windows remains unsupported because native process replacement is unavailable/unvalidated.
- Run deterministic fake-process tests on Unix/macOS and Linux CI. The live cmux smoke is macOS-specific until cmux provides a verified Linux runtime; Linux still verifies the non-cmux path and fake protocol/control flow.

## Exact files and symbols to change

### `src/launch.rs`

1. Add a private/cargo-visible handoff result/error type with actionable variants; no public CLI flag.
2. Add `handoff_cmux_workspace(spec: &ResumeSpec) -> Result<(), CmuxHandoffError>` (Unix), delegating to a private command seam.
3. Add a small private `CmuxCommand`/runner seam only if needed to make the fixed command sequence observable without a live socket. Keep it in `launch.rs`; do not create a general process framework.
4. Keep `exec` as the actual `CommandExt::exec` boundary. Do not combine cmux mutation into integration constructors.
5. Add focused unit tests under `launch.rs` using a fake `cmux` executable/script or private fake runner.

### `src/app.rs`

In `resume_selected`, after confirmation and immediately before `launch::exec(spec)`, call `launch::handoff_cmux_workspace(spec)`. On error, emit the specific handoff diagnostic and return `EXIT_ERROR`; otherwise retain the existing exec/error path.

If implementation encapsulates the pre-exec call in a single shared `launch::handoff_and_exec` to make ordering testable, keep it private and synchronous. Do not add a generic abstraction merely to rename the two calls.

### `docs/product-design.md`

Update section 6, “Resume safety and process handoff”:

- after final validation/confirmation and before Unix exec, a verified cmux caller synchronously updates and reads back its own workspace directory;
- ordinary non-cmux invocation is unchanged;
- incomplete/mismatched cmux provenance or mutation/read-back failure exits 1 before exec;
- no selection/focus occurs;
- if exec fails after a confirmed update, cmux remains at the selected Workspace and the error is exposed.

### Files that remain unchanged

- `src/session.rs` (`ResumeSpec` already supplies T).
- `src/integration/*/resume.rs` and their launch-contract tests.
- `Cargo.toml` and `Cargo.lock` (reuse `serde_json` and `std::process`; no dependency).
- cmux-pi-orchestration validation.

## Named observable tests

Use serialized environment tests (or a single test owning env mutation) because Rust process env is global. Prefer testing a function that accepts explicit captured env/current-dir and a fake runner, leaving only one thin production wrapper around `std::env`.

1. **`no_cmux_env_is_noop`** — both IDs absent; zero runner calls; returns success. Existing non-UTF-8 `ResumeSpec` behavior remains unaffected on Unix.
2. **`partial_cmux_env_fails_without_mutation`** — each one-ID/empty-ID combination errors; no `surface.report_pwd`; no agent exec.
3. **`caller_id_mismatch_fails_without_mutation`** — fake identify differs in W or S (including a matching `.focused` object); errors before workspace list/report.
4. **`workspace_directory_mismatch_fails_without_mutation`** — caller IDs match but W's `current_directory` differs from actual origin; report is never called.
5. **`workspace_match_requires_exactly_one_id`** — zero and duplicate W objects both fail without report.
6. **`verified_caller_reports_target_before_exec`** — command log is exactly identify, pre-list, one `surface.report_pwd` with W/S/T, post-list, then fake native agent; fake agent records cwd T, native argv, inherited cmux IDs, and integration env overrides.
7. **`report_pwd_failure_prevents_exec`** — nonzero RPC status is exposed; fake agent marker is absent; no retry.
8. **`post_report_readback_mismatch_prevents_exec`** — RPC exits zero but post-list remains origin/other; fake agent marker is absent; no retry.
9. **`malformed_cmux_json_fails_closed`** — malformed/missing identify and workspace fields fail without unauthorized mutation (split into table cases).
10. **`verified_cli_path_is_reused`** — after identify, all list/RPC calls use returned `app_cli_path`, not a changed second PATH lookup.
11. **`exec_failure_after_handoff_is_not_rolled_back`** — fake read-back reaches T, native launch fails, command log contains no second report to origin, and existing launch failure is returned.
12. **`non_utf8_cmux_target_fails_without_lossy_mutation`** (`cfg(unix)`) — verified cmux context rejects non-UTF-8 T before RPC, while existing `resume_spec_preserves_non_utf8_path_and_argv` continues to cover native structures.

Run only focused tests while implementing, for example the exact `launch` test module/filter and the smallest fake-agent integration case. Project-wide suites belong to final CI, not this dispatched implementation lane.

## Live acceptance smoke

Before accepting `surface.report_pwd` as implemented behavior, run an isolated manual smoke in a disposable cmux caller workspace/surface, never this planning workspace:

1. Record `identify --json --id-format uuids`, `workspace list --json --id-format uuids`, and the currently focused/selected workspace and surface.
2. Create two existing temporary directories A and B. Start the disposable caller at A and verify caller W/S plus `W.current_directory == A`.
3. Invoke exactly one raw RPC using explicit disposable W/S and `path=B`.
4. Re-list and require only W's `current_directory` changed to B.
5. Re-identify and require caller W/S unchanged and focused/selected workspace/surface unchanged.
6. Launch a fake native agent through the implementation and require its `$PWD == B` and its argv/env remain the integration contract.
7. Close/remove only the disposable resources under their normal lifecycle.

Record exact command output in the implementation evidence. If the successful RPC response or observed state differs, update parsing to observed output only if the safety invariant still holds. If focus/selection changes, another workspace changes, the addressed caller cannot be proven, or read-back does not expose B, mark the API unsupported and stop—never use focus/select commands as a workaround.

## Implementation checklist

- [ ] Add the Unix shared handoff immediately before `exec`, after revalidation and confirmation.
- [ ] Make both-absent IDs the only no-op path.
- [ ] Reject incomplete/empty cmux provenance.
- [ ] Verify `.caller` W/S against both env IDs; never authorize from `.focused` or `selected`.
- [ ] Verify exactly one W and pre-state `current_directory == actual current_dir` before mutation.
- [ ] Use server-returned `app_cli_path` and direct argv/JSON, never a shell.
- [ ] Invoke one explicit `surface.report_pwd` for W/S/T.
- [ ] Read back exactly one W and require `current_directory == T` before exec.
- [ ] Keep `Command::current_dir(T).exec()` and integration env handling unchanged.
- [ ] Expose all failures; no retry, fallback, focus/select, rollback, or persistent state.
- [ ] Add the named focused tests and product-design note.
- [ ] Prove mutation and no-focus behavior with the disposable live smoke before acceptance.
