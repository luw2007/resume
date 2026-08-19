# Research: `opencode`

## Sources

- https://opencode.ai/docs/cli (documents `--continue`/`--session`/`--fork`; does not document the on-disk storage format).
- https://opencode.school/lessons/sessions (documents that session history is saved to disk and resumable).
- Community docs claiming `~/.local/share/opencode/storage/*.json` (https://www.codeagentswarm.com/en/guides/opencode-conversation-history, https://github.com/ramtinj95/opencode-replay) — **contradicted by the actual current SQLite schema and reproducible fixture probe**; see below.
- Actual installed OpenCode 1.18.1: `opencode --help` identifies `-s, --session` as “session id to continue”; the existing SQLite `session` table was queried read-only. Paths, IDs, and workspace values are intentionally not retained here.
- Reproducible synthetic SQLite fixture matching the actual `opencode.db` schema (`sqlite3 .tables`, `.schema session`, `.schema project`, generated rows). It is regression evidence, not a substitute for the actual-installation probe.

## Local session persistence

- Store root: `$XDG_DATA_HOME/opencode` if `XDG_DATA_HOME` is set and non-empty, otherwise `~/.local/share/opencode`.
- Override precedence: `XDG_DATA_HOME` env var, then the `~/.local/share/opencode` default. No CLI flag or config key overrides the data root.
- Format: **SQLite** (`opencode.db`, `session` table), not JSON files. An actual OpenCode 1.18.1 database has the required `session.id`, `session.directory`, and integer `session.time_updated` columns. Documentation and third-party tools (see Sources) describe a `storage/session/<project>/<id>.json` layout; a sanitized legacy-format fixture with `session_info.version = "0.9.0"` is retained only to reproduce compatibility analysis. For the probed current installation, the SQLite database is authoritative.
- Session identity field(s): `session.id` (text primary key, format `ses_<hex>`), globally unique across the whole database (not scoped per project).

## Session ↔ workspace relation

- Field(s) recording the working directory: `session.directory` (SQLite column, `text not null`).
- Stability: absolute path, recorded once. The actual probe selected live rows with an existing directory; synthetic fixtures cover multiple sessions and recency order while keeping the recorded directory stable.

## Native resume

- Documented command: `opencode --session <id>` (also `opencode run --session <id>` for headless). `--continue`/`-c` resumes the *last* session (wrong session for a specific pick); `--fork` branches instead of resuming; neither is used.
- Fixture contract: the fake-native resume test asserts the discrete argv `opencode --session <id>`.
- Actual-installation command: OpenCode 1.18.1 help reports `-s, --session` as “session id to continue”.
- Isolation/profile concept, if any: none observed. OpenCode has no analogue to OMP's profiles or Claude's `CLAUDE_CONFIG_DIR`.

## Install probe

- Already installed?: yes — OpenCode 1.18.1 was already installed with persisted sessions; no installation or first run was performed for this verification.
- Install command run: n/a — existing installation.
- Maintainer approval: not needed — inspection was read-only and did not install or first-run OpenCode.
- Install location: existing executable and data root observed; exact maintainer paths are not retained in repository evidence.
- Login/network required: no for the read-only inspection.
- Minimal run performed: none. The existing SQLite store and `opencode --help` were inspected read-only.
- Probed artifact path/excerpt: an existing `~/.local/share/opencode/opencode.db` `session` row supplied an opaque `ses_…` identity and absolute `directory`; the path and values are intentionally redacted.

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

## Verification (real installation)

- Side-effect confirmation: not needed — OpenCode 1.18.1 and persisted sessions existed before this run; only read-only database and help inspection occurred.
- Probed artifact: existing `~/.local/share/opencode/opencode.db`, `session` table; a non-archived row had an opaque `ses_…` identity and an existing absolute `directory`. Identifying values are redacted.
- `resume --list -a opencode --since all` output: from the real session's recorded workspace, the feature-enabled debug binary listed the actual OpenCode sessions; no `opencode_*` diagnostic was emitted.
- Workspace match confirmed: yes — membership-based confirmation: the list command ran from the recorded workspace and returned its persisted session rows. SQLite `session.directory` is the integration's `WorkspaceEvidence` source; identifying values remain redacted.
- Fake-native-executable test argv cross-checked against real native resume: yes — OpenCode 1.18.1 help reports `-s, --session` as “session id to continue”; `fake_opencode_captures_exact_cwd_and_session_argv` asserts `cwd` is the recorded workspace and argv is exactly `["--session", "<id>"]`.

## Result

- Status: `verified`
- The real local store, its session-to-workspace relation, and selected-session native resume argv are verified. Preview parsing (message-content extraction for the picker pane) is not implemented; `session.title` is surfaced. It is outside the three required support conditions.
