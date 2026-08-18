# Disposable CI-failure experiment planning ledger

## Frozen contract

Plan only. The experiment is one metadata-only `workflow_run` notifier for failed trusted `main` CI. It creates or comments on Issues and applies two labels. It creates no repair PR, agent runner, ledger, Rust module, source checkout, artifact download, secret use, or persistent state beyond GitHub Issues/labels. Delete the workflow and labels to remove it.

## Nodes

| Node | Role / model | Isolation | Attempt | Worker surface | Output | Done signal |
|---|---|---|---|---|---|---|
| MinimalWorkflowMap | architecture / claude_sub2api/claude-opus-5 | `../resume-plan-workflow` worktree | `workflow-a1` | `surface:269` | `MINIMAL_WORKFLOW_MAP.md`; exact GitHub workflow design and acceptance cases | `.done/workflow-a1` |
| DeletionRiskReview | adversarial / codex_gpt/gpt-5.6-sol | `../resume-plan-risk` worktree | `risk-a1` | `surface:270` | `DELETION_RISK_REVIEW.md`; reject accidental durable machinery or privilege expansion | `.done/risk-a1` |
