//! Optional Codex `state_5.sqlite` enrichment (Step 8).
//!
//! **This module is strictly additive and optional.** It is compiled only
//! when the `codex-sqlite` cargo feature is enabled. Codex discovery and
//! Resume work identically with or without it: the rollout JSONL is
//! authoritative for identity and Workspace, and the JSONL-only [`super::discover`]
//! path never touches this module.
//!
//! ## What it does
//!
//! `state_5.sqlite` is a derived, evolving metadata cache that Codex writes
//! alongside rollouts. It may carry a richer title, activity timestamps, and
//! archive state than the rollout JSONL. This module opens it read-only and
//! *enriches* parsed sessions with that derived data, subject to strict
//! precedence rules.
//!
//! ## Safety contract (non-negotiable)
//!
//! 1. **Read-only open.** The database is opened via a URI with `mode=ro`,
//!    using `SQLITE_OPEN_READ_ONLY | SQLITE_OPEN_URI | SQLITE_OPEN_NO_MUTEX`.
//!    `immutable=1` is never used on a live WAL database (it would bypass
//!    SQLite's locking and can corrupt a concurrently-written DB).
//! 2. **Never write, migrate, or checkpoint.** No `PRAGMA` that writes
//!    (`wal_checkpoint`, `journal_mode`, `migrate`, schema changes) is ever
//!    issued. Only read-only `PRAGMA`s such as `table_info` and `database_list`
//!    are used.
//! 3. **Detect schema instead of assuming a version.** Tables and columns are
//!    probed from `sqlite_master` and `PRAGMA table_info`; enrichment degrades
//!    silently when an expected table or column is absent.
//! 4. **JSONL is authoritative.** A row may only *enrich* (fill in a missing
//!    title or activity time). It may never replace an existing JSONL identity
//!    or Workspace. Any disagreement (different cwd, different id) produces a
//!    [`crate::session::Diagnostic`] and is dropped — identity is never
//!    overwritten by the DB.
//! 5. **Degrade silently.** DB absent, locked, corrupt, stale, or carrying an
//!    unsupported/changed schema yields no enrichment and a single summary
//!    [`SqliteOutcome`] the caller may surface as a warning. Discovery is never
//!    blocked and never raises.
//!
//! ## Precedence
//!
//! ```text
//! rollout ID (session_meta.payload.id)  ── authoritative
//! rollout Workspace (payload.cwd)       ── authoritative
//! DB title / activity time / archived   ── enrich-only (fill missing fields)
//! DB row disagreeing on id/cwd          ── diagnostic, enrichment skipped
//! ```

#![cfg(feature = "codex-sqlite")]

use std::{
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use rusqlite::{Connection, OpenFlags};

use crate::session::Diagnostic;

use super::ParsedSession;

/// Filename of the Codex state database beneath the effective `CODEX_HOME`.
pub const STATE_DB_FILENAME: &str = "state_5.sqlite";

/// Busy timeout applied to the read-only connection. Short so discovery does
/// not stall on a locked/writing DB; a busy DB degrades to "no enrichment".
const BUSY_TIMEOUT: Duration = Duration::from_millis(2000);

/// Outcome of an enrichment pass: a coarse, display-safe summary the caller
/// may render as a single warning. Never contains DB contents.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SqliteOutcome {
    /// The DB was absent. No enrichment; nothing to warn about.
    Absent,
    /// The DB was used; `enriched` sessions had at least one field filled
    /// from a matching row, and `diagnostics` lists any precedence conflicts.
    Used {
        enriched: usize,
        skipped_no_row: usize,
        diagnostics: Vec<Diagnostic>,
    },
    /// The DB exists but could not be read: locked/busy, corrupt, or an
    /// unsupported/changed schema. Enrichment was skipped entirely. The
    /// caller may surface `category` as a summary warning.
    Degraded { category: &'static str },
}

impl SqliteOutcome {
    /// True when enrichment was skipped and a summary warning is warranted.
    pub fn is_degraded(&self) -> bool {
        matches!(self, Self::Degraded { .. })
    }

    /// Render a non-verbose, display-safe summary line.
    pub fn summary(&self) -> Option<String> {
        match self {
            Self::Absent => None,
            Self::Used {
                enriched,
                skipped_no_row,
                diagnostics,
            } => {
                if *enriched == 0 && *skipped_no_row == 0 && diagnostics.is_empty() {
                    None
                } else {
                    Some(format!(
                        "codex_sqlite_enriched: {} sessions enriched, {} without a matching row, {} precedence diagnostics",
                        enriched,
                        skipped_no_row,
                        diagnostics.len()
                    ))
                }
            }
            Self::Degraded { category } => Some(format!(
                "codex_sqlite_degraded: {category} (falling back to JSONL)"
            )),
        }
    }
}

/// Path to the state DB beneath an effective root.
pub fn state_db_path(effective_root: &Path) -> PathBuf {
    effective_root.join(STATE_DB_FILENAME)
}

/// Enrich a slice of parsed sessions from the optional state DB.
///
/// This is the public entrypoint. It never returns an error: any failure to
/// read the DB (absent, locked, corrupt, unsupported schema) is captured as a
/// [`SqliteOutcome::Degraded`] and the sessions are left exactly as the JSONL
/// produced them. The rollout JSONL remains authoritative in every case.
///
/// Enrichment is in-place: matching sessions may gain a title (only when the
/// JSONL-derived title is empty) or an activity/archived badge, and
/// disagreement diagnostics are appended to the returned outcome.
pub fn enrich(sessions: &mut [ParsedSession], effective_root: &Path) -> SqliteOutcome {
    let db_path = state_db_path(effective_root);
    if !db_path.is_file() {
        return SqliteOutcome::Absent;
    }

    let conn = match open_readonly(&db_path) {
        Ok(conn) => conn,
        Err(OpenError::Locked) => return SqliteOutcome::Degraded { category: "locked" },
        Err(OpenError::Corrupt) => {
            return SqliteOutcome::Degraded {
                category: "corrupt",
            };
        }
        Err(OpenError::Other(_)) => {
            return SqliteOutcome::Degraded {
                category: "unreadable",
            };
        }
    };

    let schema = match detect_schema(&conn) {
        Ok(schema) => schema,
        Err(DetectError::Corrupt) => {
            return SqliteOutcome::Degraded {
                category: "corrupt",
            };
        }
        Err(DetectError::Other(_)) => {
            return SqliteOutcome::Degraded {
                category: "schema_unreadable",
            };
        }
    };

    if !schema.is_supported() {
        return SqliteOutcome::Degraded {
            category: "unsupported_schema",
        };
    }

    let rows = match read_rows(&conn, &schema) {
        Ok(rows) => rows,
        Err(ReadError::Corrupt) => {
            return SqliteOutcome::Degraded {
                category: "corrupt",
            };
        }
        Err(ReadError::Other(_)) => {
            return SqliteOutcome::Degraded {
                category: "query_failed",
            };
        }
    };

    apply_rows(sessions, &rows)
}

// ---------------------------------------------------------------------------
// Opening
// ---------------------------------------------------------------------------

/// Open the database read-only with a short busy timeout.
///
/// Uses a URI with `mode=ro`. `immutable=1` is deliberately **not** set: the
/// DB may be a live WAL database concurrently written by Codex, and
/// `immutable=1` bypasses SQLite's locking, which can return garbage or
/// corrupt a writer. Instead we rely on `mode=ro` plus a short `busy_timeout`
/// so a transiently locked DB is retried briefly and then degrades.
fn open_readonly(path: &Path) -> Result<Connection, OpenError> {
    // Build a file: URI with mode=ro. Percent-encode nothing fancy: Codex
    // paths are filesystem paths without query characters; if a path somehow
    // contains a '?' it is treated as unreadable rather than risk mis-opening.
    let path_str = path.to_str().ok_or(OpenError::Other("non-utf8 path"))?;
    if path_str.contains('?') {
        return Err(OpenError::Other("path contains '?'"));
    }
    let uri = format!("file:{path_str}?mode=ro");

    // SAFETY of flags: READ_ONLY prevents any write; URI is required for the
    // `mode=ro` query param to take effect; NO_MUTEX is rusqlite's default
    // threading mode and is safe because this connection is used from one
    // thread only. Crucially, CREATE is NOT set, so a missing DB errors
    // rather than being created.
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY
        | OpenFlags::SQLITE_OPEN_URI
        | OpenFlags::SQLITE_OPEN_NO_MUTEX;

    let conn = Connection::open_with_flags(&uri, flags).map_err(|e| classify_open(&e))?;

    // Apply a short busy timeout on the connection. This configures the
    // connection's busy handler only; it does NOT issue a write PRAGMA and
    // does not modify the database file.
    let _ = conn.busy_timeout(BUSY_TIMEOUT);

    Ok(conn)
}

/// Classified open error so the caller can produce the right degradation
/// category without leaking DB internals.
#[derive(Debug)]
#[allow(dead_code)]
enum OpenError {
    /// DB is locked/busy and did not yield within the busy timeout.
    Locked,
    /// The file exists but is not a valid SQLite database (corrupt or empty).
    Corrupt,
    /// Any other failure (permissions, I/O, non-utf8 path).
    Other(&'static str),
}

fn classify_open(e: &rusqlite::Error) -> OpenError {
    use rusqlite::ffi::ErrorCode;
    match e {
        // database is locked / database disk image is malformed.
        rusqlite::Error::SqliteFailure(err, _) => match err.code {
            ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked => OpenError::Locked,
            ErrorCode::CannotOpen => OpenError::Other("cantopen"),
            ErrorCode::NotADatabase => OpenError::Corrupt,
            ErrorCode::DatabaseCorrupt => OpenError::Corrupt,
            ErrorCode::NotFound => OpenError::Other("notfound"),
            _ => OpenError::Other("sqlite_open_error"),
        },
        // rusqlite reports "file is not a database" / "file is encrypted or is
        // not a database" via SqliteFailure in most builds, but guard a string
        // fallback too.
        _ if e.to_string().contains("not a database") => OpenError::Corrupt,
        _ => OpenError::Other("open_failed"),
    }
}

// ---------------------------------------------------------------------------
// Schema detection
// ---------------------------------------------------------------------------

/// The columns/tables we care about, resolved dynamically. A `Supported`
/// schema has at least one table carrying a rollout-path-like column and an
/// identity column, so we can match rows to sessions safely.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct Schema {
    /// Name of the table that holds per-rollout metadata, if any candidate
    /// table was found.
    table: Option<String>,
    /// Column that stores the rollout JSONL path (preferred join key).
    rollout_path_col: Option<String>,
    /// Column that stores the native thread/session id.
    id_col: Option<String>,
    /// Column that stores the recorded workspace (cwd), if present.
    cwd_col: Option<String>,
    /// Column that stores a derived title, if present.
    title_col: Option<String>,
    /// Column that stores an activity/updated timestamp, if present.
    activity_time_col: Option<String>,
    /// Column marking archived sessions, if present.
    archived_col: Option<String>,
}

impl Schema {
    /// A schema is supported when we have at least one reliable join key
    /// (rollout path or id) so we never enrich by guessing.
    fn is_supported(&self) -> bool {
        self.table.is_some() && (self.rollout_path_col.is_some() || self.id_col.is_some())
    }
}

enum DetectError {
    Corrupt,
    #[allow(dead_code)]
    Other(String),
}

/// Detect the available schema by reading `sqlite_master` and per-table
/// `PRAGMA table_info` (both read-only). Candidate table names are chosen to
/// match Codex's known/evolving naming without hard-coding a version.
fn detect_schema(conn: &Connection) -> Result<Schema, DetectError> {
    // List user tables (type='table'), excluding SQLite's internal tables.
    let table_names: Vec<String> = conn
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'")
        .map_err(|e| detect_err(&e))?
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|e| detect_err(&e))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| detect_err(&e))?;

    let mut schema = Schema::default();
    for name in table_names {
        let columns = table_columns(conn, &name)?;
        // Heuristic: pick the first candidate table that exposes a rollout
        // path column or an id column together with at least one enrichment
        // column. This keeps us schema-version-tolerant.
        let rollout_path_col = first_match(
            &columns,
            &["rollout_path", "path", "file", "transcript_path"],
        );
        let id_col = first_match(
            &columns,
            &["thread_id", "session_id", "id", "rollout_id", "native_id"],
        );
        let cwd_col = first_match(&columns, &["cwd", "workspace", "workspace_path"]);
        let title_col = first_match(&columns, &["title", "name", "summary"]);
        let activity_time_col = first_match(
            &columns,
            &[
                "updated_at",
                "last_activity",
                "activity_time",
                "modified_at",
                "timestamp",
            ],
        );
        let archived_col = first_match(&columns, &["archived", "is_archived"]);

        if rollout_path_col.is_some() || id_col.is_some() {
            schema = Schema {
                table: Some(name),
                rollout_path_col,
                id_col,
                cwd_col,
                title_col,
                activity_time_col,
                archived_col,
            };
            break;
        }
    }
    Ok(schema)
}

/// Read column names for a table via `PRAGMA table_info` (read-only).
fn table_columns(conn: &Connection, table: &str) -> Result<Vec<String>, DetectError> {
    // Table name comes from sqlite_master, but defend against injection by
    // rejecting anything other than identifiers.
    if !is_identifier(table) {
        return Err(DetectError::Other("non-identifier table name".into()));
    }
    let sql = format!("PRAGMA table_info({table})");
    let names = conn
        .prepare(&sql)
        .map_err(|e| detect_err(&e))?
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|e| detect_err(&e))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| detect_err(&e))?;
    Ok(names)
}

fn is_identifier(name: &str) -> bool {
    !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn first_match(columns: &[String], candidates: &[&str]) -> Option<String> {
    for candidate in candidates {
        if let Some(col) = columns.iter().find(|c| c == candidate) {
            return Some(col.clone());
        }
    }
    None
}

fn detect_err(e: &rusqlite::Error) -> DetectError {
    use rusqlite::ffi::ErrorCode;
    if let rusqlite::Error::SqliteFailure(err, _) = e
        && matches!(
            err.code,
            ErrorCode::DatabaseCorrupt | ErrorCode::NotADatabase
        )
    {
        return DetectError::Corrupt;
    }
    DetectError::Other(e.to_string())
}

// ---------------------------------------------------------------------------
// Reading rows
// ---------------------------------------------------------------------------

/// One enrichment row matched to a session. All fields are optional; only
/// fields that both exist in the schema and have a non-null value are set.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct EnrichRow {
    /// Rollout path as stored in the DB (may be relative or absolute).
    rollout_path: Option<String>,
    /// Native thread/session id as stored in the DB.
    id: Option<String>,
    /// Recorded workspace (cwd) as stored in the DB.
    cwd: Option<String>,
    /// Derived title.
    title: Option<String>,
    /// Activity/updated timestamp.
    activity_time: Option<SystemTime>,
    /// Archived flag (truthy if present and non-zero/non-"false").
    archived: Option<bool>,
}

enum ReadError {
    Corrupt,
    #[allow(dead_code)]
    Other(String),
}

/// Read all enrichment rows from the detected table. Each row becomes one
/// `EnrichRow`; matching to sessions happens in [`apply_rows`].
fn read_rows(conn: &Connection, schema: &Schema) -> Result<Vec<EnrichRow>, ReadError> {
    let Some(table) = &schema.table else {
        return Ok(Vec::new());
    };
    // Build a column list limited to what the schema offers. Quoted because
    // we already validated the table name is a bare identifier; column names
    // came from PRAGMA table_info so they are safe but quote for safety.
    let mut cols: Vec<String> = Vec::new();
    let mut select = |schema_col: &Option<String>, slot: &str| {
        if let Some(c) = schema_col {
            cols.push(format!("\"{c}\" AS {slot}"));
        } else {
            cols.push(format!("NULL AS {slot}"));
        }
    };
    select(&schema.rollout_path_col, "rollout_path");
    select(&schema.id_col, "id");
    select(&schema.cwd_col, "cwd");
    select(&schema.title_col, "title");
    select(&schema.activity_time_col, "activity_time");
    select(&schema.archived_col, "archived");

    let sql = format!("SELECT {} FROM {table}", cols.join(", "));
    let mut stmt = conn.prepare(&sql).map_err(|e| read_err(&e))?;
    let rows = stmt
        .query_map([], |row| {
            let activity_time = row
                .get::<_, Option<rusqlite::types::Value>>(4)?
                .and_then(activity_time_from_value);
            let archived_raw: Option<rusqlite::types::Value> = row.get(5)?;
            let archived = match archived_raw {
                Some(rusqlite::types::Value::Integer(i)) => Some(i != 0),
                Some(rusqlite::types::Value::Text(s)) => {
                    Some(matches!(s.to_lowercase().as_str(), "1" | "true" | "yes"))
                }
                _ => None,
            };
            Ok(EnrichRow {
                rollout_path: row.get::<_, Option<String>>(0)?,
                id: row.get::<_, Option<String>>(1)?,
                cwd: row.get::<_, Option<String>>(2)?,
                title: row.get::<_, Option<String>>(3)?,
                activity_time,
                archived,
            })
        })
        .map_err(|e| read_err(&e))?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(|e| read_err(&e))?);
    }
    Ok(out)
}

/// Decode the heterogeneous timestamp encodings used by Codex state DB
/// revisions. Unrecognised values are optional metadata, so omit only that
/// field rather than discarding all usable rows.
fn activity_time_from_value(value: rusqlite::types::Value) -> Option<SystemTime> {
    match value {
        rusqlite::types::Value::Text(value) => crate::time::parse_iso8601(&value),
        rusqlite::types::Value::Integer(value) => epoch_time(value as f64),
        rusqlite::types::Value::Real(value) => epoch_time(value),
        rusqlite::types::Value::Null | rusqlite::types::Value::Blob(_) => None,
    }
}

fn epoch_time(value: f64) -> Option<SystemTime> {
    if !value.is_finite() || value < 0.0 {
        return None;
    }
    if value >= 1e12 {
        UNIX_EPOCH.checked_add(Duration::from_millis(value as u64))
    } else {
        UNIX_EPOCH.checked_add(Duration::from_secs_f64(value))
    }
}

fn read_err(e: &rusqlite::Error) -> ReadError {
    use rusqlite::ffi::ErrorCode;
    if let rusqlite::Error::SqliteFailure(err, _) = e
        && matches!(
            err.code,
            ErrorCode::DatabaseCorrupt | ErrorCode::NotADatabase
        )
    {
        return ReadError::Corrupt;
    }
    ReadError::Other(e.to_string())
}

// ---------------------------------------------------------------------------
// Applying rows to parsed sessions (with precedence)
// ---------------------------------------------------------------------------

/// Match DB rows to sessions and apply enrich-only fields. Returns the
/// outcome summary including any precedence diagnostics.
fn apply_rows(sessions: &mut [ParsedSession], rows: &[EnrichRow]) -> SqliteOutcome {
    let mut enriched = 0usize;
    let mut skipped_no_row = 0usize;
    let mut diagnostics = Vec::new();

    for session in sessions.iter_mut() {
        let Some(row) = match_row(rows, session) else {
            skipped_no_row += 1;
            continue;
        };

        // ---- Precedence checks: JSONL is authoritative ----

        // If the DB row carries an id that disagrees with the authoritative
        // rollout id, do not enrich this row at all; record a diagnostic.
        if let Some(db_id) = &row.id
            && !db_id.is_empty()
            && db_id != &session.id
        {
            diagnostics.push(diagnostic(
                "codex_sqlite_id_mismatch",
                &session.rollout_path,
                "DB row id disagrees with authoritative rollout id; enrichment skipped",
            ));
            continue;
        }

        // If the DB row carries a cwd that disagrees with the authoritative
        // Workspace, record a diagnostic and skip enrichment of this row.
        if let (Some(db_cwd), Some(session_cwd)) = (&row.cwd, &session.cwd)
            && !paths_equivalent(db_cwd, session_cwd)
        {
            diagnostics.push(diagnostic(
                "codex_sqlite_workspace_mismatch",
                &session.rollout_path,
                "DB row cwd disagrees with authoritative rollout workspace; enrichment skipped",
            ));
            continue;
        }

        // ---- Enrich-only: fill missing title ----
        // The JSONL-derived title is set later in build_session via
        // summarize(user_messages). We stash the DB title as a fallback hint
        // on the parsed session so build_session can prefer the JSONL
        // summary and only fall back to the DB title when there are no user
        // messages. See [`ParsedSession::sqlite_title`].
        let mut did_enrich = false;
        if session.user_messages.is_empty()
            && let Some(title) = row.title.as_deref().filter(|t| !t.trim().is_empty())
        {
            session.sqlite_title = Some(title.to_string());
            did_enrich = true;
        }

        // Enrich-only: activity time (JSONL currently has no reliable activity
        // time, so this is additive metadata, surfaced as a badge only).
        if let Some(time) = row.activity_time {
            session.sqlite_activity_time = Some(time);
            did_enrich = true;
        }

        // Enrich-only: archived flag. The scan root already sets `archived`
        // when the rollout is under archived_sessions; the DB flag is
        // informational and may not override the filesystem-derived value.
        if let Some(true) = row.archived {
            // Only record when the filesystem did not already mark it.
            if !session.archived {
                session.sqlite_archived_hint = Some(true);
                did_enrich = true;
            }
        }

        if did_enrich {
            enriched += 1;
        } else {
            skipped_no_row += 1;
        }
    }

    SqliteOutcome::Used {
        enriched,
        skipped_no_row,
        diagnostics,
    }
}

/// Find the best-matching row for a session. Preference order:
/// 1. Exact rollout path match (canonical or basename).
/// 2. Native id match (only when the row also carries a cwd that agrees, or
///    no cwd at all, to avoid id collisions across workspaces).
fn match_row<'a>(rows: &'a [EnrichRow], session: &ParsedSession) -> Option<&'a EnrichRow> {
    let session_path = session.rollout_path.to_string_lossy();
    let session_basename = session
        .rollout_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");

    // 1. Path match (exact, then basename).
    if let Some(row) = rows
        .iter()
        .find(|r| r.rollout_path.as_deref() == Some(session_path.as_ref()))
    {
        return Some(row);
    }
    if !session_basename.is_empty()
        && let Some(row) = rows.iter().filter(|r| r.rollout_path.is_some()).find(|r| {
            r.rollout_path
                .as_deref()
                .map(|p| basename(p) == session_basename)
                .unwrap_or(false)
        })
    {
        return Some(row);
    }

    // 2. Id match with cwd agreement.
    if let Some(row) = rows
        .iter()
        .find(|r| r.id.as_deref() == Some(session.id.as_str()))
    {
        // Require cwd agreement when both sides have one.
        if let (Some(db_cwd), Some(session_cwd)) = (&row.cwd, &session.cwd) {
            if paths_equivalent(db_cwd, session_cwd) {
                return Some(row);
            }
        } else if row.cwd.is_none() {
            return Some(row);
        }
    }

    None
}

/// Compare two workspace path strings for equivalence, tolerating trailing
/// slashes and differing canonicalization (one side may be canonical, the
/// other not). Compares the lossy string forms after trimming trailing
/// separators.
fn paths_equivalent(a: &str, b: &Path) -> bool {
    let normalize = |s: &str| s.trim_end_matches('/').to_string();
    let a_norm = normalize(a);
    let b_norm = normalize(&b.to_string_lossy());
    if a_norm == b_norm {
        return true;
    }
    // Fall back to canonical comparison if possible.
    Path::new(&a_norm)
        .canonicalize()
        .ok()
        .zip(b.canonicalize().ok())
        .is_some_and(|(ac, bc)| ac == bc)
}

fn basename(p: &str) -> &str {
    match p.rsplit_once('/') {
        Some((_, base)) => base,
        None => p,
    }
}

fn diagnostic(category: &'static str, path: &Path, chain: &str) -> Diagnostic {
    Diagnostic {
        category,
        count: 1,
        verbose_path: Some(path.to_path_buf()),
        verbose_chain: Some(chain.to_string()),
    }
}

/// Helper exposed for tests that need to assert the DB was opened read-only.
/// Returns true when the connection refuses writes — the definition of
/// read-only we actually care about. The production enrichment path never
/// calls this; it is test-only.
#[cfg(test)]
pub(crate) fn assert_readonly(conn: &Connection) -> bool {
    // A read-only connection rejects writes outright. This is the behavior we
    // must guarantee; checking `PRAGMA query_only` is NOT sufficient because
    // `mode=ro` blocks writes at the VFS layer without setting query_only.
    conn.execute("CREATE TABLE __resume_readonly_probe (x INTEGER)", [])
        .is_err()
}

#[cfg(test)]
mod tests;
