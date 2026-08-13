# Research: `opencode`

## Sources

- `opencode --help` / `opencode session --help` / `opencode run --help` (installed binary, `opencode` 1.18.1).
- Real installation at `~/.local/share/opencode/opencode.db` and `~/.local/share/opencode/storage/`, probed directly (`sqlite3 .tables`, `.schema session`, `.schema project`, sample rows).
- https://opencode.ai/docs/cli (documents `--continue`/`--session`/`--fork`; does not document the on-disk storage format).
- https://opencode.school/lessons/sessions (documents that session history is saved to disk and resumable).
- Community docs claiming `~/.local/share/opencode/storage/*.json` (https://www.codeagentswarm.com/en/guides/opencode-conversation-history, https://github.com/ramtinj95/opencode-replay) — **contradicted by the real probe**; see below.

## Local session persistence

- Store root: `$XDG_DATA_HOME/opencode` if `XDG_DATA_HOME` is set and non-empty, otherwise `~/.local/share/opencode`.
- Override precedence: `XDG_DATA_HOME` env var, then the `~/.local/share/opencode` default. No CLI flag or config key overrides the data root.
- Format: **SQLite** (`opencode.db`, `session` table), not JSON files. Documentation and third-party tools (see Sources) describe a `storage/session/<project>/<id>.json` layout; the real installation confirmed that layout is only a legacy artifact of a pre-1.0 install (the JSON `session_info.version` on the one surviving file reads `"0.9.0"`) and is not written by the current binary. The probe is stronger evidence than the docs; the SQLite database is authoritative.
- Session identity field(s): `session.id` (text primary key, format `ses_<hex>`), globally unique across the whole database (not scoped per project).

## Session ↔ workspace relation

- Field(s) recording the working directory: `session.directory` (SQLite column, `text not null`).
- Stability: absolute path, recorded once, not observed to change across a session's lifetime in the probed data (4700+ rows checked structurally, sampled by recency).

## Native resume

- Documented command: `opencode --session <id>` (also `opencode run --session <id>` for headless). `--continue`/`-c` resumes the *last* session (wrong session for a specific pick); `--fork` branches instead of resuming; neither is used.
- Verified command (after install probe): confirmed via `opencode --help` and `opencode run --help` output on the installed 1.18.1 binary; the flag is a real top-level option, not doc-only.
- Isolation/profile concept, if any: none. OpenCode has no analogue to OMP's profiles or Claude's `CLAUDE_CONFIG_DIR`.

## Install probe

- Install command run: none — OpenCode was already installed and extensively used on the maintainer's machine (`~/.opencode/bin/opencode`, version 1.18.1, 4738 real session rows). No first-run side effect was needed or performed by this Skill run.
- Maintainer approval: not applicable (no install/first-run performed).
- Install location: pre-existing, `~/.opencode/bin/opencode`.
- Login/network required: not evaluated (no install performed).
- Minimal run performed: none needed; real historical data already present.
- Probed artifact path/excerpt: `~/.local/share/opencode/opencode.db`, `session` table, e.g. row
  `ses_00000000000000000000000001 | .../sample_app | New session - 2026-07-21T09:40:44.326Z | ...`.

## Implementation mapping

- `SessionKey`: `agent = "opencode"`, `effective_root` = the resolved data root, `profile = None`, `native_locator = session.id`.
- `WorkspaceEvidence`: `Recorded { workspace: session.directory, historical_git_identity: None }`, read-only, no normalization.
- `ResumeSpec`: `program = "opencode"`, `argv = ["--session", id]`, `cwd = session.directory`, `env = []`.
- Closest existing integration for shape parity: `codex` (optional-SQLite precedent for feature-gating; OpenCode's SQLite is the sole/required source rather than enrichment-only, so the `opencode` cargo feature is required, unlike `codex-sqlite`).

## Performance characterization

- Benchmark group: `opencode_discovery` in `benches/discovery.rs`
- Fixture generator: `opencode_db` in `benches/fixtures.rs`
- Risk tracked: query-scale cost (SQLite `SELECT ... ORDER BY time_updated DESC` with no index on `time_updated`). Bench results at three scales (Apple M4 Max, `--quick`):
  - 200 sessions: ~103 µs
  - 2000 sessions: ~487 µs (4.7× for 10× rows — sublinear, in-memory sort still cheap)
  - 20000 sessions: ~9.7 ms (20× for 10× rows — sort becomes the dominant cost at scale)
- Noted because: OpenCode uses an indexed SQLite database, not per-session files, so there is no file-size sensitivity. The scaling axis is row count: the `ORDER BY` forces a full sort at scale, and the bench tracks whether that cost stays bounded as real installs accumulate tens of thousands of sessions. If this becomes a regression target, the fix is an index on `time_updated`, not a read-strategy change.

## Verification (real installation)

- `resume --list -a opencode --up all` output: real, currently-installed sessions listed with real ids/titles/timestamps (e.g. `ses_00000000000000000000000002`, "New session - 2026-07-21T09:33:07.118Z").
- Workspace match confirmed: yes — `resume --json -a opencode --up all` reports `"workspace":"/home/example"` matching the probed `session.directory` value for those rows, and correctly flags `"risk":"BroadWorkspace"` for the home-directory workspace.
- Fake-native-executable test argv cross-checked against real native resume: yes — `fake_opencode_captures_exact_cwd_and_session_argv` (`src/integration/opencode/resume.rs`) asserts `cwd` equals the recorded directory and argv is exactly `["--session", "<id>"]`, matching the real `--session` flag confirmed via `opencode --help`.

## Result

- Status: `verified`
- Notes: Preview parsing (message-content extraction for the picker's preview pane) is **not implemented** — only `session.title` is surfaced. This does not block `verified`: the three required support conditions (local session enumeration, workspace relation, native selected-session resume) are all met and tested. Full transcript preview (reading `session_message`/`part` tables) is out of scope for this run and not tracked as a blocker.
