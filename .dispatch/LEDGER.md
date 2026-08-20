# Disposable CI-failure experiment planning ledger

## Frozen contract

Plan only. The experiment is one metadata-only `workflow_run` notifier for failed trusted `main` CI. It creates or comments on Issues and applies two labels. It creates no repair PR, agent runner, ledger, Rust module, source checkout, artifact download, secret use, or persistent state beyond GitHub Issues/labels. Delete the workflow and labels to remove it.

## Nodes

| Node | Role / model | Isolation | Attempt | Worker surface | Output | Done signal |
|---|---|---|---|---|---|---|
| MinimalWorkflowMap | architecture / claude_sub2api/claude-opus-5 | `../resume-plan-workflow` worktree | `workflow-a1` | `surface:269` | Accepted with adversarial narrowing: `5163125688c7e001961cfae99445326af9f98fdd`; its job/step/API/manual-dispatch proposal was rejected | `.done/workflow-a1` observed |
| DeletionRiskReview | adversarial / codex_gpt/gpt-5.6-sol | `../resume-plan-risk` worktree | `risk-a1` | `surface:270` | Accepted: `414d735e3087c7a6bd5c45cef7dc4d34f879ccfc`; reduced metadata-only pilot adopted | `.done/risk-a1` observed |

| CmuxResumeHandoffPlan | planner / codex_gpt/gpt-5.6-sol | `../resume` shared planning tree | `cmux-handoff-plan-a1` | `D698D7D6-B11B-4345-B772-FCCB6ED0C1ED`, `λ ⠧ resume` | Accepted: `e47de8f97dbf28b442931e04eded6838d8577b82`; require isolated live `surface.report_pwd` smoke before production acceptance | `.done/cmux-handoff-plan-a1` observed |
| CmuxResumeHandoffDetails | change-detail / claude_sub2api/claude-opus-5 | `../resume` shared planning tree | `cmux-handoff-details-a1` | `0A686DE3-969E-4713-BE7A-9C6C3945E55B`, `λ ⠧ resume` | Accepted: `8f9a248eb4b1cf88f5029dabdd2363f9cf460197`; live smoke verified `surface.report_pwd`, no focus/selection mutation, canonical target required | `.done/cmux-handoff-details-a1` observed |
| CmuxResumeHandoffImplement | implementation / codex_gpt/gpt-5.6-luna:max | `../resume-cmux-handoff` worktree | `cmux-handoff-implement-a1` | `EC530FEF-F5D0-4CD7-878F-A20E538069C2`, `λ ⠹ resume` | Accepted production handoff implementation and regression matrix | `.done/cmux-handoff-implement-a1` observed |

| CmuxResumeHandoffReview | review / claude_sub2api/claude-sonnet-5 | `../resume` shared review tree | `cmux-handoff-review-a1` | `90E058ED-5CF0-4D4C-B15E-806A8E39C19E`, `λ ⠋ resume` | Findings accepted and closed by `CmuxResumeHandoffReviewFix` | `.done/cmux-handoff-review-a1` observed |

| CmuxResumeHandoffReviewFix | implementation / codex_gpt/gpt-5.6-luna:max | `../resume-cmux-handoff` worktree | `cmux-handoff-review-fix-a1` | `EC530FEF-F5D0-4CD7-878F-A20E538069C2`, `λ ⠹ resume` | Accepted corrections: environment-mutating tests isolated in child processes; full suite 515 passed | `.done/cmux-handoff-review-fix-a1` observed |

| CmuxResumeHandoffE2E | implementation / codex_gpt/gpt-5.6-luna:max | `../resume-cmux-handoff` worktree | `cmux-handoff-e2e-a1` | `EC530FEF-F5D0-4CD7-878F-A20E538069C2`, `λ ⠹ resume` | Accepted: real PTY picker → Pi native process; verifies A→B handoff, exact cmux protocol, in-agent binding, and report-failure no-launch | `.done/cmux-handoff-e2e-a1` observed |
