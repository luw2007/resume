# Target
Produce the implementation plan for the `resume`/cmux workspace-handoff defect. Read-only investigation only; do not edit source, documentation, or project configuration. Do not run formatters, linters, or project-wide suites.

# Problem
`resume` correctly sets `Command::current_dir(&ResumeSpec.cwd)` before Unix `exec`, so the replacement native agent starts in the selected Session workspace. cmux's caller workspace `current_directory` remains the directory where `resume` was launched. A later cmux-pi-orchestration dispatch refuses its mandatory binding check because `CMUX_WORKSPACE_ID` identifies the old cmux workspace while `$PWD` is now the selected Session workspace.

# Inputs
- `src/launch.rs`, `src/app.rs`, `src/session.rs`, `src/integration/*/resume.rs`
- `docs/product-design.md`
- `skill://cmux-pi-orchestration` (especially platform verification and lifecycle)
- Live `cmux --help` subcommands as needed; do not infer undocumented argv.

# Output
Write `CMUX_RESUME_HANDOFF_PLAN.md` in this worktree. It must identify the exact safe state transition, its ordering relative to `exec`, environment and failure semantics, Unix/macOS/Linux scope, test seams, and an explicit no-op/non-cmux path. Prefer no dependency and no persistent state. State exact files/symbols to change and named observable tests.

# Constraints
- Preserve `resume` as a native process-replacing launcher.
- Do not weaken cmux-pi-orchestration workspace-binding validation.
- Never switch or focus a cmux workspace/pane/tab.
- No async, retry loop, fallback behavior that conceals mutation failures, or new abstraction unless evidence requires it.
- The handoff must not mutate cmux unless this process is the verified caller of the cmux workspace it would mutate.

# Completion
Commit the report. Write `.done/cmux-handoff-plan-a1` containing its commit SHA.

# Verification
Ground every cmux command and field in live CLI output or observed runtime state. Clearly separate verified behavior from inference.