# Target
Implement the approved `resume`/cmux workspace-handoff fix after both upstream reports are present at:
- `../resume/.dispatch/CMUX_RESUME_HANDOFF_PLAN.md`
- `../resume/.dispatch/CMUX_RESUME_HANDOFF_CHANGE_DETAILS.md`

# Problem
After a user selects a Session, `resume` uses `Command::current_dir(&ResumeSpec.cwd).exec()` so the native replacement agent runs from the Session workspace. cmux's caller workspace metadata remains at the pre-launch directory, causing strict cmux-pi-orchestration binding validation to block subsequent worker dispatch.

# Change
Implement the exact agreed change details. Update the smallest production surface, targeted tests, and user-facing documentation only when the reports establish it is required. Add a regression test that fails before the change and demonstrates the relevant state-transition contract; run it red before production edits, then green after. Preserve the native process-replacement behavior and final native agent cwd.

# Constraints
- Do not weaken cmux-pi-orchestration's binding validation.
- Mutate cmux only after verifying the current process is cmux's caller and only for that caller workspace.
- Never focus/select a workspace/pane/tab.
- Non-cmux invocations must preserve existing behavior without requiring cmux.
- Fail before `exec` if a verified required cmux handoff mutation fails; do not hide failures, retry, or invent fallback behavior.
- No unrelated formatting, refactors, dependencies, abstraction layers, or broad test runs.

# Completion
Commit source, tests, and required docs. Write `.done/cmux-handoff-implement-a1` containing the final commit SHA.

# Verification
Run the focused regression test(s) and the smallest relevant smoke command. Report exact commands and output in the commit body or `.done`-adjacent final note.