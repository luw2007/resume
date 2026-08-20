# cmux workspace handoff — review

Read-only review of the current `resume` cmux workspace-handoff implementation
(`git log` HEAD `12021ff`, series `15c3d5b..12021ff`) against
`CMUX_RESUME_HANDOFF_PLAN.md`, `CMUX_RESUME_HANDOFF_CHANGE_DETAILS.md`,
`src/launch.rs`, `src/app.rs`, and `docs/product-design.md`.

## Verdict: FAIL — blocking issues found

The control-flow and safety-guard *code* matches the accepted plan/change
details closely (correct call ordering, canonicalized target, byte-exact
read-back, no focus/select/rollback/retry, `app_cli_path` reuse, bounded
stderr). The blocking problem is test coverage: the committed tests exercise
only the pure `handoff_with` helper with hand-supplied inputs, never the
actual production entry point or its wiring into the exec path, and several
named required tests are simply absent rather than merely reworded.

## Acceptance checklist

| Acceptance item | Result |
|---|---|
| Both absent cmux IDs performs no lookup or mutation | PASS at the `handoff_with` level; **not verified** at the production `handoff_cmux_workspace` entry point (see Blocker 1) |
| Partial/empty IDs fail before mutation | PASS (`cmux_handoff_rejects_incomplete_and_malformed_states`, `src/launch.rs:633`) |
| Verified path: identify → pre-list → one addressed `surface.report_pwd` → post-list → **before native `exec`** | Call sequencing itself is PASS (`cmux_handoff_protocol_regression_matrix`, `src/launch.rs:602`); the "before native `exec`" half is **not tested** (see Blocker 2) |
| No focus/select/rollback/retry; `app_cli_path` reused | PASS — no such calls exist in `src/launch.rs`, and `cmux_handoff_protocol_regression_matrix` asserts calls 2–4 all target the identify-returned `app_cli_path` (`src/launch.rs:610-612`) |
| Tests prove named semantics, not just fixture exhaustion | **FAIL** — three guard branches have zero tests at all (Major 1–3), not merely imprecise ones |
| Error paths fail-closed; diagnostics bounded | PASS — every subprocess-status error path truncates stderr via `bounded_stderr` (1 KiB, `src/launch.rs:401-405`); JSON-parse failures carry only fixed short reasons, never raw stdout/session content |

## Blocking findings

### Blocker 1 — Production entry point and its `resume_selected` wiring are never exercised by a test

`src/launch.rs:447` (`handoff_cmux_workspace`) is the only function the rest of
the program calls (`src/app.rs:490`), but every cmux test in `src/launch.rs`
(`no_cmux_env_is_noop`, `cmux_handoff_protocol_regression_matrix`,
`cmux_handoff_rejects_incomplete_and_malformed_states`,
`cmux_handoff_rejects_mismatch_duplicate_and_failures`) calls the private pure
helper `handoff_with` directly with explicit `Option<&OsStr>`/`Path` arguments
and a `MockRunner`. None of them go through `handoff_cmux_workspace`, so none
of the following production-only code is covered by any test:

- reading `CMUX_WORKSPACE_ID`/`CMUX_SURFACE_ID` via `std::env::var_os`
  (`src/launch.rs:448-449`);
- the `command_available(OsStr::new("cmux"))` PATH gate and its
  `CmuxHandoffError::CliUnavailable` branch (`src/launch.rs:453-454`);
- `std::env::current_dir()` as `origin` and `spec.cwd` as `target`
  (`src/launch.rs:459-464`);
- the insertion point and short-circuit in `resume_selected`
  (`src/app.rs:489-493`) — no test in `src/app.rs` sets
  `CMUX_WORKSPACE_ID`/`CMUX_SURFACE_ID` at all, so there is no evidence the
  handoff actually runs (or is actually skipped) at the call site the plan
  targets.

A wiring defect here (swapped origin/target, wrong env var name, wrong spec
field, or the call moved to the wrong side of `launch::exec`) would not be
caught by `cargo test`.

**Smallest correction**: add one focused `src/app.rs` test that sets
`CMUX_WORKSPACE_ID`/`CMUX_SURFACE_ID` (serialized, since env is process-global)
to a syntactically valid but non-cmux value, and asserts `resume_selected`
fails closed with the `cmux workspace handoff failed` diagnostic before
reaching `launch::exec`'s error text — proving the call is actually wired in,
in the right order, at `src/app.rs:490`.

### Blocker 2 — No test proves the handoff runs *before* native `exec`, or that a handoff failure prevents `exec`

The plan's own named-test list (`CMUX_RESUME_HANDOFF_PLAN.md` §"Named
observable tests" #6, #7, #8, #11; restated in
`CMUX_RESUME_HANDOFF_CHANGE_DETAILS.md` §8 tests 6–8, 11) requires a fake
native agent so the test can observe that:

- on success, the agent is launched with `cwd == target` only *after* the
  full identify/list/report/list sequence (`verified_caller_reports_target_before_exec`);
- on `ReportStatus` failure, no agent process marker exists
  (`report_pwd_failure_prevents_exec`);
- on `ReadbackMismatch`, no agent process marker exists
  (`post_report_readback_mismatch_prevents_exec`);
- if `exec` itself fails after a successful handoff, no second/rollback
  report is sent (`exec_failure_after_handoff_is_not_rolled_back`).

No such fake-agent-integration test exists anywhere in the diff. The four
`src/launch.rs` cmux tests assert only `MockRunner` call counts/order for the
cmux protocol itself; they never invoke `launch::exec` or any process
standing in for the resumed agent. Consequently "before native `exec`" and
"failure prevents `exec`" are current-state *code-review* conclusions (the
call sits above `launch::exec(spec)` at `src/app.rs:494`), not test-proven
facts — exactly the category the review brief calls out ("tests prove their
named semantics rather than merely exhausting fixtures").

**Smallest correction**: add one `src/app.rs` (or `src/launch.rs`) test using
the repository's existing fake-executable pattern (e.g.
`src/integration/pi/tests/resume.rs:225`'s `fake_pi`/marker-file approach) that
runs `resume_selected` end-to-end with cmux env set, a `MockRunner`-equivalent
or fake `cmux` script, and a fake `program` that writes a marker file with its
`cwd`; assert the marker is written with the handed-off target on success and
is absent on `ReportStatus`/`ReadbackMismatch` failure.

## Major findings (non-blocking to the control-flow correctness, but named-required and currently zero-coverage)

### Major 1 — `NonUtf8Target` branch is completely untested

`src/launch.rs:355-358` rejects a canonicalized target that isn't valid UTF-8
before issuing the RPC. This is explicitly required test #12 in both planning
docs (`non_utf8_cmux_target_fails_without_lossy_mutation`) precisely because
it is a security-relevant fail-closed guard against lossy path mutation. No
test constructs a non-UTF-8 path and asserts `NonUtf8Target` with zero
`report`/list calls after it.

**Smallest correction**: add a `#[cfg(unix)]` test building a target `PathBuf`
from invalid UTF-8 bytes via `OsStringExt::from_vec`, call `handoff_with` with
a runner primed only through the pre-list step, and assert
`Err(CmuxHandoffError::NonUtf8Target)` with no report call in the capture log.

### Major 2 — `CliPathUnavailable` branch is completely untested

`src/launch.rs:330-338` requires `app_cli_path` from the identify response to
be present, non-empty, and pass the local `executable()` check before it is
used for the list/report/read-back calls. No test supplies a missing, empty,
or non-executable `app_cli_path` and asserts `CliPathUnavailable` with the
list/report calls never made.

**Smallest correction**: extend
`cmux_handoff_rejects_incomplete_and_malformed_states` with a small table:
identify JSON missing `app_cli_path`, identify JSON with `app_cli_path` set to
a non-existent file — each asserting `CliPathUnavailable` and exactly one
recorded call (identify only).

### Major 3 — `CliUnavailable` (PATH-resolution) branch is completely untested

`src/launch.rs:453-454`, the `command_available(OsStr::new("cmux"))` check
guarding the very first call, has no test. It is cheap to cover without a live
`cmux` binary by temporarily restricting `PATH` in a serialized test.

**Smallest correction**: add a serialized test that clears `PATH` (or points
it at an empty temp dir) with both cmux env vars set, calls
`handoff_cmux_workspace`, and asserts `CliUnavailable`. Combining this with
Blocker 1's fix (a first real test of `handoff_cmux_workspace` itself) covers
both gaps in one place.

## Minor findings

### Minor 1 — Duplicate `#[cfg(unix)]` attribute

`src/launch.rs:542-544`:

```rust
    #[cfg(unix)]
    #[cfg(unix)]
    #[test]
    fn no_cmux_env_is_noop() {
```

Harmless (both attributes agree), but indicates the diff wasn't cleaned up
before commit.

**Smallest correction**: delete one of the two `#[cfg(unix)]` lines.

### Minor 2 — Byte-exact (non-canonicalized) read-back invariant is not directly pinned

`CMUX_RESUME_HANDOFF_CHANGE_DETAILS.md` §2.5/§3.2 establishes, as a specific
load-bearing design decision, that the read-back comparison must be exact
string equality against the sent (canonicalized) path — the implementation
must *not* canonicalize the value read back from cmux a second time. The code
correctly does this (`src/launch.rs:376-382`: `actual` from `one_workspace`
is compared directly, uncanonicalized, against the already-canonicalized
`target`). But the only existing `ReadbackMismatch` test
(`cmux_handoff_rejects_mismatch_duplicate_and_failures`,
`src/launch.rs:715-726`) exercises a stale-origin read-back, not a
canonicalization-variant of the correct target (e.g., a trailing slash), so it
does not specifically pin "read-back is exact, not lenient" — a regression
that re-canonicalized `actual` before comparing would still pass every
existing test.

**Smallest correction**: add the plan's named test #14
(`readback_requires_exact_string_match`): after a successful report to
canonical target `T`, have the post-list return `T` with an appended trailing
slash and assert `ReadbackMismatch` rather than success.

### Minor 3 — No symlink-specific fixture for the pre-state canonicalization guard

`CMUX_RESUME_HANDOFF_CHANGE_DETAILS.md` §3.1 documents, with a concrete
reproduction, why the pre-state comparison must canonicalize both
`std::env::current_dir()` and cmux's reported `current_directory` (logical
`$PWD` vs. physical `getcwd()` diverge under a symlinked working tree). The
code does call `std::fs::canonicalize` on both sides (`src/launch.rs:295`,
`:343`), but no test builds an actual symlinked directory pair to prove the
guard tolerates the logical/physical spelling difference (named test #13,
`symlinked_workspace_reports_canonical_path`). Existing tests only pass
already-canonical `tempdir()` paths, so a regression that compared raw
(non-canonicalized) origin/reported-directory strings could pass all existing
tests on a filesystem without symlinked temp roots.

**Smallest correction**: add a `#[cfg(unix)]` test with
`std::os::unix::fs::symlink`, one canonical real directory, and one symlinked
alias; run `handoff_with` with the origin passed via the symlinked spelling
and the pre-list `current_directory` reported via the canonical spelling (or
vice versa); assert the pre-state check still succeeds.

## Scope note

No source, docs, or task files were modified during this review. This report
and `.done/cmux-handoff-review-a1` are the only artifacts written.
