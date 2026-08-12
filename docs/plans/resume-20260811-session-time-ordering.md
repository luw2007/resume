# Session time ordering implementation plan

## Objective

Make the shared Session ordering place the newest recorded `updated_at` first while preserving every other Session behavior.

## Inputs

- [`docs/superpowers/specs/2026-08-11-session-time-ordering-design.md`](../superpowers/specs/2026-08-11-session-time-ordering-design.md): approved ordering contract.
- [`src/session.rs`](../../src/session.rs): shared `compare_sessions` implementation and unit tests.

## Work

### 1. Replace the shared sort key

In `src/session.rs`, change `compare_sessions` to compare `Session::updated_at` rather than `ActivityStatus` rank or observation time.

Compare `Option<UpdateTime>` by `UpdateTime::at` descending. A present update time sorts before an absent one. Keep `SessionKey` ascending as the final deterministic tie-breaker.

Remove `activity_rank` and `activity_time` because they no longer serve the comparison. Do not change `ActivityStatus`, `UpdateTime`, `sort_sessions`, call sites, filtering, or output formatting.

### 2. Replace the ordering test

Rename the existing activity-priority test to describe time-descending ordering. Build sessions with both `updated_at` and varying activity states, then prove that:

- a newer update sorts before an older update regardless of activity state;
- a known update sorts before an absent update;
- equal and absent updates use the ascending key tie-break.

### 3. Verify

Run the focused ordering test, then run:

```sh
cargo test --all-features --locked
```

The full suite must pass. No UI, JSON, filtering, or launch contract is changed.
