# Target
Add a real PTY E2E regression for the cmux workspace handoff in `/Users/luwei.will/ai/resume`. Work in the existing isolated worktree `/Users/luwei.will/ai/resume-cmux-handoff`, rebased/reset to the current main working branch before edits.

# Required scenario
The test must run the compiled `resume` binary from source workspace A, provide exactly one discoverable supported Pi Session whose recorded workspace is B, and drive the real production picker with Enter over a PTY. It must use a fake cmux CLI and fake Pi native agent, each real executable subprocess.

# Assertions
- The picker renders the Pi candidate and Enter selects it.
- The fake cmux observes `identify → workspace list → rpc surface.report_pwd → workspace list`, with explicit caller W/S and canonical B.
- The fake Pi runs with cwd canonical B.
- While Pi runs, its own binding check observes fake cmux workspace state equals canonical B; it must emit a durable marker.
- A report failure exits `resume` nonzero and no fake-Pi marker appears.
- This test models cmux caller workspace A vs selected Session B; do not merely call `handoff_with` or test-only seam.

# Isolation
- Follow existing PTY gating (`SPIKE_PTY_TESTS`); skip cleanly when unavailable.
- The spawned `resume` process has fully isolated HOME/XDG/agent roots/PATH/cmux state. No test-process global cwd/environment mutation.
- Preserve repository PTY deadline conventions: wait-for expected candidate, then an absence/failure assertion with a deadline; no fixed one-second reads.

# Constraints
No production code change unless the E2E exposes a real defect. No dependencies, no unrelated refactors, no broad suite mid-flight.

# Verification
First prove the test fails against parent behavior if practical; then run the new test at least twice, `cargo fmt --check`, `cargo clippy --all-targets --all-features --locked -- -D warnings`, and finally `cargo test --all-features --locked`.

# Completion
Commit test-only changes. Write `.done/cmux-handoff-e2e-a1` containing final commit SHA; worktree clean.