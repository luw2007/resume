# Session time ordering design

## Goal

Order discovered Sessions by their recorded update time with the newest Session first. Change ordering only; preserve all existing Session fields, filtering, activity detection, rendering, and launch behavior.

## Ordering contract

`compare_sessions` is the shared ordering authority for picker candidates and deterministic `--list` / `--json` output. It will compare Sessions in this order:

1. `updated_at.at` descending: newer timestamps sort first.
2. A Session with `updated_at: Some(_)` sorts before one with `updated_at: None`.
3. Equal timestamps, or two absent timestamps, sort by `SessionKey` ascending to preserve deterministic output.

`updated_at` remains the existing integration-owned timestamp: agent-native activity time when available, otherwise transcript-file modification time. `UpdateTimeSource` remains metadata and is not a sort key.

## Non-goals

- No changes to `ActivityStatus`, including Active/Inactive/Unknown semantics.
- No changes to `--since` filtering, which already independently uses `updated_at`.
- No changes to picker display, text list columns, JSON schema, or launch selection identity.
- No new configuration option or CLI flag.

## Tests

Replace the current activity-priority ordering test with a unit test that proves:

- a newer `updated_at` sorts before an older one even when activity states differ;
- a known update time sorts before an unknown update time;
- equal and absent update times resolve by `SessionKey` ascending.

## Verification

Run the focused session-ordering test and `cargo test --all-features --locked`. The test must prove time-descending ordering without changing any other observable contract.
