# Research: `opencode`

## Sources

- Captured `opencode --help` / `opencode session --help` / `opencode run --help` output for `opencode` 1.18.1.
- Reproducible synthetic SQLite fixture matching the observed `opencode.db` schema (`sqlite3 .tables`, `.schema session`, `.schema project`, generated rows).
- https://opencode.ai/docs/cli (documents `--continue`/`--session`/`--fork`; does not document the on-disk storage format).
- https://opencode.school/lessons/sessions (documents that session history is saved to disk and resumable).
- Community docs claiming `~/.local/share/opencode/storage/*.json` (https://www.codeagentswarm.com/en/guides/opencode-conversation-history, https://github.com/ramtinj95/opencode-replay) — **contradicted by the current SQLite schema and reproducible fixture probe**; see below.

## Local session persistence

- Store root: `$XDG_DATA_HOME/opencode` if `XDG_DATA_HOME` is set and non-empty, otherwise `~/.local/share/opencode`.
- Override precedence: `XDG_DATA_HOME` env var, then the `~/.local/share/opencode` default. No CLI flag or config key overrides the data root.
- Format: **SQLite** (`opencode.db`, `session` table), not JSON files. Documentation and third-party tools (see Sources) describe a `storage/session/<project>/<id>.json` layout; a sanitized legacy-format fixture with `session_info.version = "0.9.0"` is retained only to reproduce compatibility analysis. For the tested current schema, the SQLite database is authoritative.
- Session identity field(s): `session.id` (text primary key, format `ses_<hex>`), globally unique across the whole database (not scoped per project).

## Session ↔ workspace relation

- Field(s) recording the working directory: `session.directory` (SQLite column, `text not null`).
- Stability: absolute path, recorded once. Synthetic fixtures cover multiple sessions and recency order while keeping the recorded directory stable.

## Native resume

- Documented command: `opencode --session <id>` (also `opencode run --session <id>` for headless). `--continue`/`-c` resumes the *last* session (wrong session for a specific pick); `--fork` branches instead of resuming; neither is used.
- Verified command: confirmed via captured `opencode --help` and `opencode run --help` output for 1.18.1; the flag is a real top-level option, not doc-only.
- Isolation/profile concept, if any: none. OpenCode has no analogue to OMP's profiles or Claude's `CLAUDE_CONFIG_DIR`.

## Reproducible schema probe

- Install command run: none. This evidence does not depend on, or claim access to, a maintainer installation.
- Maintainer approval: not applicable (no install or first run performed).
- Login/network required: no; the probe uses a local synthetic fixture.
- Reproduction: create a temporary `$XDG_DATA_HOME/opencode/opencode.db` with the fixture generator used by the tests, then inspect it with `sqlite3 .tables`, `.schema session`, and a deterministic query ordered by `time_updated`.
- Synthetic artifact excerpt (`$TMPDIR/opencode/opencode.db`, `session` table):
  `ses_00000000000000000000000001 | /home/example/projects/sample_app | Synthetic session | 1700000000000`.

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
- Noted because: OpenCode uses an indexed SQLite database, not per-session files, so there is no file-size sensitivity. The scaling axis is row count: the `ORDER BY` forces a full sort at scale, and the bench tracks whether that cost stays bounded as databases accumulate tens of thousands of sessions. If this becomes a regression target, the fix is an index on `time_updated`, not a read-strategy change.

## Verification (synthetic and reproducible)

- `resume --list -a opencode --up all` is exercised against generated SQLite fixtures with clearly synthetic ids, titles, timestamps, and directories.
- Workspace match confirmed: yes — JSON output reports `"workspace":"/home/example/projects/sample_app"` matching the fixture's `session.directory`; a separate generic home-directory fixture verifies `"risk":"BroadWorkspace"`.
- Fake-native-executable argv verification: `fake_opencode_captures_exact_cwd_and_session_argv` (`src/integration/opencode/resume.rs`) asserts `cwd` equals the recorded fixture directory and argv is exactly `["--session", "<id>"]`, matching the documented `--session` flag and captured `opencode --help` output.

## Result

- Status: `verified`
- Notes: Verification is based on reproducible synthetic fixtures, captured CLI help, and fake-executable tests; it makes no claim about a specific maintainer installation. Preview parsing (message-content extraction for the picker's preview pane) is **not implemented** — only `session.title` is surfaced. This does not block `verified`: the three required support conditions (local session enumeration, workspace relation, native selected-session resume) are all met and tested. Full transcript preview (reading `session_message`/`part` tables) is out of scope for this run and not tracked as a blocker.
