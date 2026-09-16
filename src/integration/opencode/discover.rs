use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OpenFlags, OptionalExtension};

use crate::session::{
    ActivityStatus, Session, SessionKey, SupportStatus, UpdateTime, UpdateTimeSource,
    WorkspaceEvidence,
};

use super::AGENT;

/// Busy timeout applied to the read-only connection. Short so discovery does
/// not stall on a locked/writing database; a busy database degrades to no
/// Sessions plus a diagnostic rather than blocking.
const BUSY_TIMEOUT: Duration = Duration::from_millis(2000);

/// One OpenCode session read from the `session` table, ready to become a
/// [`crate::session::Session`] via [`ParsedSession::into_session`] and a
/// [`crate::session::ResumeSpec`] via [`super::resume_spec`].
#[derive(Clone, Debug)]
pub struct ParsedSession {
    pub id: String,
    pub directory: PathBuf,
    pub title: Option<String>,
    pub updated_at: Option<SystemTime>,
    /// Native `session.parent_id` value; `None` for top-level sessions.
    pub parent_id: Option<String>,
    /// `session.model` is a JSON object (`{"id", "providerID", "variant"}`);
    /// this is its bare `id` field.
    pub final_model: Option<String>,
    /// Sum of `session.tokens_{input,output,reasoning,cache_read,cache_write}`.
    pub tokens: Option<u64>,
}

impl ParsedSession {
    pub fn into_session(
        self,
        effective_root: &std::path::Path,
        home: Option<&std::path::Path>,
    ) -> Session {
        let workspace = WorkspaceEvidence::Recorded {
            workspace: self.directory,
            historical_git_identity: None,
        };
        let risk = crate::scope::broad_workspace_risk(&workspace, home);
        Session {
            key: SessionKey {
                agent: AGENT.into(),
                effective_root: effective_root.to_path_buf(),
                profile: None,
                native_locator: self.id.clone().into(),
            },
            resumable_id: self.id.into(),
            title: self.title,
            final_model: self.final_model,
            tokens: self.tokens,
            updated_at: self.updated_at.map(|at| UpdateTime {
                at,
                source: UpdateTimeSource::Native,
            }),
            workspace,
            support: SupportStatus::Supported,
            // OpenCode discovery never correlates a live process; Unknown,
            // never Inactive, matching the "positive evidence only" contract.
            activity: ActivityStatus::Unknown,
            risk,
        }
    }
}

/// Outcome of discovering OpenCode sessions.
#[derive(Clone, Debug, Default)]
pub struct DiscoverOutcome {
    pub parsed: Vec<ParsedSession>,
    /// Count of rows skipped because they carried no directory, or a
    /// directory that is not an absolute path (never guessed at).
    pub skipped_rows: usize,
}

/// Diagnostic category for `Ok(None)` in this build: the database file is
/// genuinely absent, matching every other integration's missing-root handling.
pub const NO_SESSIONS_CATEGORY: &str = "opencode_root_unavailable";

/// Discover OpenCode sessions from the SQLite database beneath
/// `effective_root`. Opens the database read-only and never writes,
/// migrates, or checkpoints it. Returns `Ok(None)` when the database file
/// does not exist (OpenCode not installed, or never run) rather than an
/// error — callers surface that as `opencode_root_unavailable`, matching
/// every other integration's missing-root handling.
pub fn discover(effective_root: &std::path::Path) -> rusqlite::Result<Option<DiscoverOutcome>> {
    let path = super::roots::db_path(effective_root);
    if !path.is_file() {
        return Ok(None);
    }
    let conn = open_readonly(&path)?;
    let mut outcome = DiscoverOutcome::default();
    let mut stmt = conn.prepare(
        "select id, directory, title, time_updated, parent_id, model, \
         tokens_input, tokens_output, tokens_reasoning, tokens_cache_read, tokens_cache_write \
         from session order by time_updated desc",
    )?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let id: String = row.get(0)?;
        let directory: String = row.get::<_, Option<String>>(1)?.unwrap_or_default();
        let title: Option<String> = row.get(2)?;
        let updated_millis: Option<i64> = row.get(3)?;
        let parent_id: Option<String> = row.get(4)?;
        let model_json: Option<String> = row.get(5)?;
        let final_model = model_json.as_deref().and_then(model_id_from_json);
        let tokens_input: i64 = row.get(6)?;
        let tokens_output: i64 = row.get(7)?;
        let tokens_reasoning: i64 = row.get(8)?;
        let tokens_cache_read: i64 = row.get(9)?;
        let tokens_cache_write: i64 = row.get(10)?;
        let tokens_sum = tokens_input
            + tokens_output
            + tokens_reasoning
            + tokens_cache_read
            + tokens_cache_write;
        let tokens = (tokens_sum > 0).then_some(tokens_sum as u64);
        let directory = PathBuf::from(directory);
        if !directory.is_absolute() {
            outcome.skipped_rows += 1;
            continue;
        }
        outcome.parsed.push(ParsedSession {
            id,
            directory,
            title: title.filter(|title| !title.is_empty()),
            updated_at: updated_millis.and_then(millis_to_system_time),
            parent_id,
            final_model,
            tokens,
        });
    }
    Ok(Some(outcome))
}

/// Extract the bare `id` field from `session.model`'s JSON object
/// (`{"id", "providerID", "variant"}`). Malformed or non-object JSON yields
/// `None` rather than a discovery error — the column is best-effort display
/// metadata, never load-bearing for resume itself.
fn model_id_from_json(raw: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(raw)
        .ok()?
        .get("id")?
        .as_str()
        .map(str::to_owned)
}

fn millis_to_system_time(millis: i64) -> Option<SystemTime> {
    u64::try_from(millis)
        .ok()
        .map(|millis| UNIX_EPOCH + Duration::from_millis(millis))
}

/// Open the database read-only with a short busy timeout.
///
/// Uses `SQLITE_OPEN_READ_ONLY | SQLITE_OPEN_NO_MUTEX` (no URI `immutable=1`:
/// the database may be a live database concurrently written by OpenCode, and
/// `immutable=1` bypasses SQLite's locking, which can return garbage or
/// corrupt a writer). A short `busy_timeout` retries a transiently locked
/// database briefly and then surfaces the lock as an error.
fn open_readonly(path: &std::path::Path) -> rusqlite::Result<Connection> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.busy_timeout(BUSY_TIMEOUT)?;
    // Confirm the `session` table exists before the caller's `prepare`, so a
    // pre-1.0 database (JSON-only storage layout, no `session` table) is
    // reported the same way as a missing database rather than a query error.
    let exists: Option<i64> = conn
        .query_row(
            "select 1 from sqlite_master where type = 'table' and name = 'session'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    if exists.is_none() {
        return Err(rusqlite::Error::QueryReturnedNoRows);
    }
    Ok(conn)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn create_test_db(path: &std::path::Path) {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(
            "create table project (id text primary key, worktree text not null);
             create table session (
                 id text primary key,
                 project_id text not null,
                 directory text not null,
                 title text not null,
                 time_created integer not null,
                 time_updated integer not null,
                 parent_id text,
                 model text,
                 tokens_input integer default 0 not null,
                 tokens_output integer default 0 not null,
                 tokens_reasoning integer default 0 not null,
                 tokens_cache_read integer default 0 not null,
                 tokens_cache_write integer default 0 not null
             );",
        )
        .unwrap();
        conn.execute(
            "insert into project (id, worktree) values ('proj_1', '/Users/test/work')",
            [],
        )
        .unwrap();
        conn.execute(
            "insert into session (id, project_id, directory, title, time_created, time_updated)
             values ('ses_1', 'proj_1', '/Users/test/work', 'Fix the bug', 1700000000000, 1700000100000)",
            [],
        )
        .unwrap();
        conn.execute(
            "insert into session (id, project_id, directory, title, time_created, time_updated)
             values ('ses_2', 'proj_1', 'relative/path', '', 1700000000000, 1700000100000)",
            [],
        )
        .unwrap();
    }

    #[test]
    fn discovers_sessions_and_skips_relative_directory() {
        let dir = tempdir().unwrap();
        let db = dir.path().join(super::super::roots::DB_FILENAME);
        create_test_db(&db);
        let outcome = discover(dir.path()).unwrap().unwrap();
        assert_eq!(outcome.parsed.len(), 1);
        assert_eq!(outcome.skipped_rows, 1);
        let session = &outcome.parsed[0];
        assert_eq!(session.id, "ses_1");
        assert_eq!(session.directory, PathBuf::from("/Users/test/work"));
        assert_eq!(session.title.as_deref(), Some("Fix the bug"));
        assert!(session.updated_at.is_some());
        assert!(session.parent_id.is_none());
    }

    #[test]
    fn extracts_bare_model_id_and_sums_token_columns() {
        let dir = tempdir().unwrap();
        let db = dir.path().join(super::super::roots::DB_FILENAME);
        create_test_db(&db);
        let conn = Connection::open(&db).unwrap();
        conn.execute(
            "insert into session (id, project_id, directory, title, time_created, time_updated, model, tokens_input, tokens_output, tokens_reasoning, tokens_cache_read, tokens_cache_write)
             values ('ses_metered', 'proj_1', '/Users/test/work', 'Metered', 1700000000000, 1700000300000, '{\"id\":\"gpt-5.6-sol\",\"providerID\":\"codex_gpt\",\"variant\":\"default\"}', 100, 20, 5, 3, 2)",
            [],
        )
        .unwrap();
        let outcome = discover(dir.path()).unwrap().unwrap();
        let session = outcome
            .parsed
            .iter()
            .find(|session| session.id == "ses_metered")
            .unwrap();
        assert_eq!(session.final_model.as_deref(), Some("gpt-5.6-sol"));
        assert_eq!(session.tokens, Some(130));
    }

    #[test]
    fn preserves_native_parent_id() {
        let dir = tempdir().unwrap();
        let db = dir.path().join(super::super::roots::DB_FILENAME);
        create_test_db(&db);
        let conn = Connection::open(&db).unwrap();
        conn.execute(
            "insert into session (id, project_id, directory, title, time_created, time_updated, parent_id)
             values ('ses_child', 'proj_1', '/Users/test/work', 'Subtask', 1700000000000, 1700000200000, 'ses_1')",
            [],
        )
        .unwrap();
        let outcome = discover(dir.path()).unwrap().unwrap();
        let child = outcome
            .parsed
            .iter()
            .find(|session| session.id == "ses_child")
            .unwrap();
        assert_eq!(child.parent_id.as_deref(), Some("ses_1"));
    }

    #[test]
    fn missing_database_returns_none() {
        let dir = tempdir().unwrap();
        assert!(discover(dir.path()).unwrap().is_none());
    }

    #[test]
    fn pre_1_0_database_without_session_table_is_treated_as_unavailable() {
        let dir = tempdir().unwrap();
        let db = dir.path().join(super::super::roots::DB_FILENAME);
        Connection::open(&db)
            .unwrap()
            .execute_batch("create table migration (v integer)")
            .unwrap();
        assert!(discover(dir.path()).is_err());
    }

    #[test]
    fn readonly_connection_rejects_writes() {
        let dir = tempdir().unwrap();
        let db = dir.path().join(super::super::roots::DB_FILENAME);
        create_test_db(&db);
        let conn = open_readonly(&db).unwrap();
        let result = conn.execute("delete from session", []);
        assert!(result.is_err(), "read-only connection must reject writes");
    }
}
