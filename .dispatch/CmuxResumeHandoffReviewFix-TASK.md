# Target
Implement every blocker and major/minor regression gap in `CMUX_RESUME_HANDOFF_REVIEW.md` on the current `resume` branch. Work only in `/Users/luwei.will/ai/resume`; do not alter unrelated untracked files.

# Required corrections
1. Add a focused production-entry/wiring test proving `resume_selected` calls cmux handoff before `launch::exec`, and that a verified-cmux handoff failure emits `resume: cmux workspace handoff failed:` rather than reaching the agent launch error. Serialize environment mutation and restore it.
2. Add fake-cmux plus fake-native-agent coverage for successful ordered handoff then exec; ReportStatus and ReadbackMismatch must leave the agent marker absent; failed native exec after successful handoff must issue no rollback.
3. Cover `CliUnavailable`, `CliPathUnavailable` (missing/non-executable), `NonUtf8Target`, malformed list data, exact readback mismatch with trailing slash, and symlinked canonical origin/target.
4. Delete duplicate `#[cfg(unix)]`.

# Constraints
- Preserve strict handoff behavior: no focus/select, retry, fallback, or rollback.
- Tests must prove each named behavior directly—do not make fixture exhaustion look like a semantic assertion.
- Use existing project fake executable patterns. No dependencies or broad refactors.

# Verification
Run red tests before each fix where practical; then `cargo test --locked launch::tests::`, the new focused app test(s), `cargo fmt --check`, and `cargo clippy --all-targets --all-features --locked -- -D warnings`.

# Completion
Commit source/tests only. Write `.done/cmux-handoff-review-fix-a1` containing the final commit SHA and leave the worktree clean.