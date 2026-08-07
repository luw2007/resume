# Session format and resume behavior research

Verified read-only against locally installed CLIs and sanitized local storage on 2026-08-07. No real message bodies, identifiers, repository remotes, credentials, or private URLs are included here.

## Summary

| Agent | Installed version | Primary store | Stable identity | Authoritative Workspace | Exact native Resume |
|---|---:|---|---|---|---|
| Codex | 0.146.0 | JSONL rollouts under `CODEX_HOME/sessions`; optional SQLite metadata | `session_meta.payload.id` | `session_meta.payload.cwd` | `codex -C <workspace> resume <uuid>` |
| Claude Code | 2.1.220 | heterogeneous JSONL under `CLAUDE_CONFIG_DIR/projects` | top-level UUID filename confirmed by embedded `sessionId` | event `cwd` | `claude --resume <uuid>` from Workspace |
| Pi | 0.84.1 | v1-v3 JSONL under the effective Pi session root | header `id` | header `cwd` | `pi --session <absolute-jsonl-path>` |
| OMP | 17.2.10 | v3 JSONL under default or named-profile agent roots | header `id`, scoped by profile/root | header `cwd` | `omp [--profile <name>] --resume <id>` |

All integrations must discover and parse independently. Shared code is limited to pure helpers such as safe JSONL streaming, text normalization, path handling, and launch execution.

## Codex

### Storage

- Root: `CODEX_HOME`, defaulting to `~/.codex`.
- Rollouts: `sessions/YYYY/MM/DD/rollout-...-<uuid>.jsonl`.
- Archived rollouts: `archived_sessions`.
- Optional metadata/cache: `state_5.sqlite`, `session_index.jsonl`, and `history.jsonl`.
- `config.toml` and named CLI config profiles are not reliable Session identity. Preserve a nondefault `CODEX_HOME` on Resume.

The rollout JSONL is authoritative. SQLite and indexes may enrich title, preview, timestamps, Git metadata, and archived state, but may be absent or stale.

### Format

Records use `{timestamp, type, payload}`. The normal first record is `type = "session_meta"`; important payload fields include `id`, `cwd`, `timestamp`, `originator`, `cli_version`, `source`, `thread_source`, `model_provider`, `git`, and `parent_thread_id`.

Use `payload.id`, not the filename or unrelated `payload.session_id`, as the stable ID. Use primary `payload.cwd`; `workspace_roots` are additional roots, not the Resume directory.

User input may be represented twice: as `event_msg` with `payload.type = "user_message"`, and as a user `response_item` message with `input_text` or attachment blocks. The adapter must deduplicate these representations and exclude system/developer injection.

### Resume and activity

Execute `codex -C <workspace> resume <uuid>`, preserving `CODEX_HOME` when nondefault. Codex 0.146.0 does not document Resume by rollout path.

A live Codex process holding a rollout file descriptor is positive but nonexclusive activity evidence. Absence of an open descriptor is not evidence of inactivity.

### Required fixtures

Modern and older rollout shapes, imported and archived sessions, absent/stale SQLite metadata, filename/ID mismatch, alternate `CODEX_HOME`, duplicate user representations, attachment input, unknown events, malformed header, and truncated tail.

## Claude Code

### Storage

- Root: `CLAUDE_CONFIG_DIR`, defaulting to `~/.claude`.
- Transcripts: `projects/<workspace-key>/<session-id>.jsonl`.

The workspace key replaces non-alphanumeric characters and is collision-prone. Never reverse it to infer Workspace. Parse top-level transcripts and use embedded `cwd`; ignore nested `subagents` as independent Workspace sessions.

### Format

There is no file-level schema version. Heterogeneous events include `user`, `assistant`, `ai-title`, `agent-name`, `last-prompt`, `mode`, and permission metadata. Event `version` is a producer version, not a schema contract.

Accept only a top-level UUID-named transcript whose embedded `sessionId` agrees. Per-event `uuid` is not the resumable Session ID.

User content may be a string or typed blocks. Include human text; exclude `tool_result`. Titles may be present as `agent-name`/`agentName` and `ai-title`/`aiTitle`; explicit name should precede generated title, with first valid user input as fallback. This precedence needs an isolated behavioral fixture because the native picker precedence was not safely invoked.

### Resume and activity

Run `claude --resume <uuid>` from the authoritative Workspace and preserve a nondefault `CLAUDE_CONFIG_DIR`. Never use `--continue` for exact Resume.

No authoritative active marker was found. Activity is unknown unless a future positive process/session association is validated.

### Required fixtures

Workspace-key collisions, UUID agreement/disagreement, string and block messages, tool-only events, title/name metadata, mixed producer versions, unknown records, empty/malformed files, and truncated tail.

## Pi

### Storage

- Agent root: `PI_CODING_AGENT_DIR`, defaulting to `~/.pi/agent`.
- Default sessions root: `<agent-root>/sessions`, grouped by encoded resolved Workspace.
- Effective custom session root precedence: `--session-dir`, `PI_CODING_AGENT_SESSION_DIR`, settings `sessionDir`, then default.

A custom root may be flat and contain multiple Workspaces; filter it by header `cwd`, not directory name.

### Format

Append-only JSONL with a `type = "session"` header. Current version is 3; readers also understand v1 and v2. Header fields include stable `id`, `timestamp`, absolute `cwd`, and optional `parentSession`. A later `session_info.name` supplies the user display name.

User entries contain `message.role = "user"`, string or typed block content, and message/entry timestamps. Include text blocks and represent images without emitting base64. Pi activity time prefers message timestamps over entry timestamps, then header time, then file mtime.

Pi may migrate old formats when opening them. `resume` must parse read-only and never invoke Pi merely to inspect a Session.

### Resume and activity

The safest exact command is `pi --session <absolute-jsonl-path>`, preserving `--session-dir <root>` when discovery used a custom root. Do not use `--session-id`: if absent, it can create a new Session.

`~/.pi/session-control` sockets can be positive evidence only when reliably tied to a Session; process presence alone is insufficient.

### Required fixtures

Versions 1, 2, and 3; named/cleared sessions; strings, text plus image, and image-only input; branched parents; alternate and flat roots; duplicate IDs across roots; timestamp fallback; malformed middle/tail records; missing header; missing Workspace; and a growing file.

## OMP

### Storage and profiles

- Base: `PI_CONFIG_DIR`, defaulting to `~/.omp`.
- Default profile agent root: `<base>/agent`.
- Named profile agent root: `<base>/profiles/<name>/agent`.
- Profile selection: `--profile`, then `OMP_PROFILE`, then `PI_PROFILE`.
- `PI_CODING_AGENT_DIR` overrides only the unprofiled agent root; named profiles deliberately ignore it.
- `--session-dir` overrides Session lookup for an invocation.
- Existing XDG OMP directories can split data/state/cache; root resolution must mirror the installed OMP behavior and be fixture-driven.

Profile and effective root are part of Session provenance and identity. Do not infer Workspace from OMP's encoded or migrated directory names when the header is readable.

### Format

JSONL normally begins with a padded title record (`type = "title"`, `v = 1`) followed by a v3 `type = "session"` header with `id`, `timestamp`, absolute `cwd`, and optional title metadata. Filenames are not authoritative.

User messages are typed envelopes with `message.role = "user"`, block content, and attribution. Use attribution to remove agent-injected inputs. `title_change` records update title state.

Imported Sessions receive a new OMP ID and a `foreign_session_import` custom entry containing source kind, origin ID/path/cwd. Resume the OMP header ID; show only a safe origin badge by default.

### Resume and activity

Default: `omp --resume <id>`. Named profile: `omp --profile <name> --resume <id>`. Add `--session-dir <root>` when discovery used it, and run from header `cwd`.

Terminal breadcrumbs map TTY names to cwd/session path but can be stale and do not contain PID. Report Active only after correlating a live OMP process, its TTY, and matching breadcrumb Session path.

### Required fixtures

Default and named profiles; base/root/profile environment interactions; custom session root; optional existing XDG split roots; title record plus v3 header; generated/named filenames; title changes; attributed injections; text/image input; foreign import metadata; duplicate IDs across profiles; live/stale breadcrumbs; missing Workspace; empty/malformed/truncated JSONL.

## Cross-integration implementation corrections

1. Launch data is `command + argv + Workspace + narrowly scoped environment overrides`. Codex and Claude may require alternate roots; OMP profile selection changes root semantics. Execution still bypasses a shell.
2. Identity must include integration plus provenance: agent, profile/effective root, and stable native ID. Cross-agent imports remain separate Sessions.
3. Workspace always comes from persisted Session data when available. Encoded storage directories are not a general inference mechanism.
4. JSONL readers retain valid records, ignore a half-written final record with a warning, isolate malformed files, accept unknown record types, and never repair or migrate.
5. Activity is positive-evidence-only. Unknown is the normal fallback.
6. SQLite is optional Codex enrichment through a read-only short-timeout connection; JSONL remains authoritative.
7. No discovery path may call an agent CLI in a mode that opens, migrates, repairs, imports, or creates a Session.
