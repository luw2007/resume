# Target
After the planner report is available at `../resume/.dispatch/CMUX_RESUME_HANDOFF_PLAN.md`, produce implementation-level change details for the `resume`/cmux workspace-handoff defect. Read-only investigation only; do not edit source, documentation, or project configuration. Do not run formatters, linters, or project-wide suites.

# Problem
`resume` launches a selected native agent using `Command::current_dir(&ResumeSpec.cwd).exec()`. cmux workspace metadata still reports the directory from which `resume` was launched. The strict cmux-pi-orchestration binding check then blocks worker creation after handoff because workspace metadata and process cwd disagree.

# Required input
- `../resume/.dispatch/CMUX_RESUME_HANDOFF_PLAN.md`
- Existing source and tests cited in that plan.
- Live cmux CLI help and runtime state where necessary.

# Output
Write `CMUX_RESUME_HANDOFF_CHANGE_DETAILS.md` in this worktree. Specify the exact Rust control flow and API boundary; every guard and error path; concrete test fixture/process strategy that verifies cmux mutation only in the validated caller context and validates the final native-agent cwd; documentation changes if public behavior changes. Flag any unsupported cmux API rather than inventing it.

# Constraints
- Preserve strict cmux orchestration validation; repair the state that validation observes.
- The only cmux mutation may update the verified caller workspace directory before process replacement.
- No focus/select commands, no persistent state, no dependency, no retries, and no best-effort suppression of a mutation error.
- Avoid touching production source: this is a detailed design artifact only.

# Completion
Commit the report. Write `.done/cmux-handoff-details-a1` containing its commit SHA.

# Verification
All command/API claims require direct CLI evidence. The output is usable verbatim by an implementation worker.