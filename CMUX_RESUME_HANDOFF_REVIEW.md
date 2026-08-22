# cmux workspace handoff — review

Read-only re-review of the current `resume` cmux workspace-handoff
implementation (HEAD `cf2addc`, review series
`12021ff..1e457a9..7bc80e8..cf2addc`) against `CMUX_RESUME_HANDOFF_PLAN.md`,
`CMUX_RESUME_HANDOFF_CHANGE_DETAILS.md`, `src/launch.rs`, `src/app.rs`,
`tests/cmux_handoff_e2e.rs`, and `docs/product-design.md`.

This supersedes the prior `FAIL` verdict recorded at commit `5fc9835`. All
Blocker/Major/Minor findings from that review have been addressed by test-only
commits (`32754e9`, `e9bf1d0`, `dd38a6c`, `ecdfb52`, `1e457a9`,
`d4c2d90`..`7bc80e8`); no behavior-affecting source lines changed
(`src/app.rs` gained a `HandoffThenExecError` match arm purely to make the
handoff→exec order observable to tests — see Note below).

## Verdict: PASS

Ran to completion locally:

- `cargo fmt --check` — clean.
- `cargo clippy --all-targets --all-features --locked -- -D warnings` — clean.
- `cargo test --locked` — **431 unit tests + 1 e2e + 4 parser-property + 15
  picker-spike + 31 step9_app = all pass, 0 failed.**
- `cargo test --locked --test cmux_handoff_e2e` — real-PTY end-to-end test
  passes independently (`cmux_resume_real_pty_handoff_success_and_report_failure`).

## Acceptance checklist

| Acceptance item | Result |
|---|---|
| Both absent cmux IDs performs no lookup or mutation | PASS — now proven at **both** layers: `handoff_with` (`no_cmux_env_is_noop`, `src/launch.rs:637`) and the production entry point (`production_entry_both_absent_is_noop`, `src/launch.rs:597`) |
| Partial/empty IDs fail before mutation | PASS (`cmux_handoff_rejects_incomplete_and_malformed_states`) |
| Verified path: identify → pre-list → one addressed `surface.report_pwd` → post-list → **before native `exec`** | PASS at both the unit level (`cmux_handoff_protocol_regression_matrix`) and now at full-binary PTY level: `tests/cmux_handoff_e2e.rs` asserts the exact 4-call log order (`identify`, `workspace list`, `rpc surface.report_pwd`, `workspace list`) through the real `resume` binary, and that a failing `rpc` yields nonzero process exit with no marker written by the resumed agent |
| No focus/select/rollback/retry; `app_cli_path` reused | PASS — unchanged from prior review; still no such calls exist anywhere in `src/launch.rs` |
| Tests prove named semantics, not just fixture exhaustion | PASS — see closed-gap detail below |
| Error paths fail-closed; diagnostics bounded | PASS — unchanged; `bounded_stderr` (`src/launch.rs:397`) still caps at 1 KiB |

## Prior Blocker/Major/Minor findings — disposition

### Blocker 1 (production entry point never exercised) — CLOSED

- `production_entry_both_absent_is_noop` (`src/launch.rs:597`) drives
  `handoff_cmux_workspace` itself with real `CMUX_WORKSPACE_ID`/`CMUX_SURFACE_ID`
  cleared, proving the no-op path at the actual production function, not just
  `handoff_with`.
- `production_entry_rejects_missing_cmux_cli` drives `handoff_cmux_workspace`
  with both env vars set and an empty `PATH`, proving the `command_available`
  PATH gate (`src/launch.rs:453-454`, unchanged) is live.
- `fake_cmux_and_native_agent_prove_order_and_fail_closed_exec` drives
  `handoff_cmux_workspace` (the exact function `src/app.rs` calls) against a
  real fake `cmux` script and a real fake agent subprocess, in a forked child
  test process (isolating the `PATH`/`CMUX_*`/cwd env mutation from the rest
  of the suite — resolves the process-global-env-safety concern implicit in
  the original finding).
- `resume_selected`'s wiring (`src/app.rs:489-496`) is now exercised
  end-to-end by `tests/cmux_handoff_e2e.rs`, which launches the actual
  `resume` binary under a PTY with `CMUX_WORKSPACE_ID`/`CMUX_SURFACE_ID` set,
  drives the picker, presses Enter, and asserts on the resulting cmux-call log
  and process exit code. This is strictly stronger evidence than the
  originally requested "one focused `src/app.rs` test" — it proves the
  call site, argument plumbing, and ordering all at once, through the real
  binary rather than a reimplementation of `resume_selected`'s control flow.

### Blocker 2 (no proof handoff precedes `exec`, or that failure blocks `exec`) — CLOSED

- `handoff_then_exec_short_circuits_and_orders` (`src/launch.rs:*`) uses an
  order-recording pure harness to prove: (a) on handoff success, `exec` runs
  after handoff and its error propagates; (b) on handoff failure, `exec` is
  never invoked at all — the exact "no fallback, no continuing to exec"
  requirement.
- `fake_cmux_and_native_agent_prove_order_and_fail_closed_exec` extends this
  with real subprocesses: a real fake agent writes a marker file only if
  actually spawned. The test proves the marker exists with the handed-off
  `cwd` on success; is **absent** when `FAIL_REPORT=1` makes the `rpc` call
  fail (`ReportStatus`); and that a subsequent `exec` failure (nonexistent
  program) still results in exactly one `rpc` call in the log (no retry, no
  second/rollback report) — closing the plan's named tests #6/#7/#11 in one
  fixture.
- `tests/cmux_handoff_e2e.rs::cmux_resume_real_pty_handoff_success_and_report_failure`
  independently reproduces both outcomes through the real `resume` binary:
  non-zero process exit and no `pi.marker` file on RPC failure; exact 4-call
  ordered log and exit 0 with the resumed agent's `$PWD` equal to the
  canonical target on success. This closes named test #8
  (`post_report_readback_mismatch_prevents_exec`)'s intent at the strongest
  available layer — an actual resumed-agent process that never launches on
  failure.

### Major 1 (`NonUtf8Target` untested) — CLOSED

`cmux_handoff_covers_target_and_symlink_guards` (`src/launch.rs`) builds a
target path from invalid UTF-8 bytes via `OsStringExt::from_vec`, asserts
`Err(CmuxHandoffError::NonUtf8Target)`, and asserts the runner's call count
stayed at 2 (identify + pre-list only — no `rpc` call after the guard),
matching the plan's named test #12 exactly.

### Major 2 (`CliPathUnavailable` untested) — CLOSED

`cmux_handoff_rejects_missing_and_nonexecutable_app_cli_path` covers both a
missing `app_cli_path` field and a present-but-non-executable file, each
asserting `CliPathUnavailable` with exactly one recorded call (identify
only — no list/report after the guard).

### Major 3 (`CliUnavailable` PATH-gate untested) — CLOSED

`production_entry_rejects_missing_cmux_cli` covers this at the production
entry point (stronger than the originally suggested `handoff_with`-level
test, since `CliUnavailable` is only reachable from
`handoff_cmux_workspace`, not from `handoff_with`).

### Minor 1 (duplicate `#[cfg(unix)]`) — CLOSED

`src/launch.rs:635-637` now has a single `#[cfg(unix)]` above
`no_cmux_env_is_noop`.

### Minor 2 (byte-exact read-back not specifically pinned) — CLOSED

`cmux_handoff_rejects_mismatch_duplicate_and_failures` was extended with a
trailing-slash variant of the correct canonical target: the post-list reports
`T + "/"` and the test asserts `ReadbackMismatch` rather than success,
directly pinning that the comparison is exact-string, not re-canonicalized.

### Minor 3 (no symlink fixture for pre-state canonicalization) — CLOSED

`cmux_handoff_covers_target_and_symlink_guards` builds a real symlinked
directory pair (`real` + `alias -> real`) and calls `handoff_with` with the
*symlinked* spelling as `origin` while the mock pre-list reports the
*canonical* spelling, asserting success — proving the pre-state guard
tolerates logical/physical divergence as `CMUX_RESUME_HANDOFF_CHANGE_DETAILS.md`
§3.1 requires.

## Note: one non-test source touch, and why it does not violate the "tests only" constraint

`src/app.rs:490-496` and `src/launch.rs` gained `HandoffThenExecError` /
`handoff_then_exec` / `handoff_then_run_with`. These are structural seams
introduced so tests can observe the handoff→exec ordering and short-circuit
behavior without reimplementing `resume_selected`'s control flow or relying on
`#[cfg(unix)]` process replacement (`exec()` never returns, so it cannot be
asserted against directly). `handoff_then_run_with` is `#[cfg(test)]`-gated
and invisible outside the test build. `handoff_then_exec`/`HandoffThenExecError`
are `pub(crate)` production code, but they are a pure refactor: the runtime
behavior is unchanged — `handoff_cmux_workspace` is still called first, its
`Err` still short-circuits to the same `cmux workspace handoff failed`
diagnostic and `EXIT_ERROR`, and `exec`'s error is still surfaced identically
via the pre-existing `eprintln!("resume: unable to launch ...")` path. This is
the same "no generic abstraction, single private wrapper for observability"
allowance the plan itself anticipated (`CMUX_RESUME_HANDOFF_PLAN.md`,
"if implementation encapsulates the pre-exec call in a single shared
`launch::handoff_and_exec`... keep it private and synchronous"). No cmux
protocol behavior, error variant, or control-flow ordering changed.

## Scope note

This review did not modify source, docs, or task files; it read the current
tree, ran `cargo fmt --check`, `cargo clippy -D warnings`, and the full
`cargo test --locked` suite (regular + e2e), and re-verified each prior
finding against the current code and tests. This report and
`.done/cmux-handoff-review-a1` are the only artifacts written by this pass.
