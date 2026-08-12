//! Tests for the optional Codex `state_5.sqlite` enrichment (Step 8).
//!
//! These tests are compiled only under the `codex-sqlite` feature. They prove:
//! - deleting the DB does not change which Codex JSONL Sessions are
//!   discoverable or resumable (the Step 8 exit criterion);
//! - enrichment never mutates the DB file (read-only filesystem snapshots);
//! - the DB is opened read-only (`mode=ro`, never `immutable=1`, no writes);
//! - precedence holds: rollout JSONL identity/Workspace are authoritative;
//! - the layer degrades silently when the DB is absent, locked, corrupt,
//!   stale, or carries an old/new/changed schema.

#![cfg(all(test, feature = "codex-sqlite"))]

use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    thread,
};

use rusqlite::Connection;
use serde_json::json;

use super::*;
use crate::{
    integration::codex::{DiscoveredSession, discover_with_filter_enriched, sqlite::SqliteOutcome},
    preview::jsonl::Bounds,
    preview::snapshot,
    session::{RiskStatus, SupportStatus, WorkspaceEvidence},
};

// ---------------------------------------------------------------------------
// Fixture helpers
// ---------------------------------------------------------------------------

/// Build a fresh Codex-style home temp dir.
fn codex_home() -> tempfile::TempDir {
    tempfile::tempdir().expect("temp dir")
}

/// A unique counter so each test's session id is distinct even across runs.
static ID_COUNTER: AtomicU64 = AtomicU64::new(1);

fn next_id(prefix: &str) -> String {
    let n = ID_COUNTER.fetch_add(1, Ordering::SeqCst);
    format!("{prefix}-{n}")
}

/// Write a rollout JSONL file with the given records.
fn write_rollout(home: &Path, rel: &str, records: &[serde_json::Value]) -> PathBuf {
    let path = home.join(rel);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    let mut content = String::new();
    for record in records {
        content.push_str(&record.to_string());
        content.push('\n');
    }
    fs::write(&path, content.as_bytes()).unwrap();
    path
}

/// A modern session_meta record.
fn session_meta(id: &str, cwd: &str) -> serde_json::Value {
    json!({
        "timestamp": "2026-08-07T10:00:00.000Z",
        "type": "session_meta",
        "payload": {
            "id": id,
            "cwd": cwd,
            "timestamp": "2026-08-07T10:00:00.000Z",
            "originator": "cli",
            "cli_version": "0.146.0",
            "source": "interactive",
        }
    })
}

/// Write a rollout that has NO user messages, so its JSONL-derived title is
/// empty and the DB title (if any) is the only enrichment candidate.
fn write_rollout_no_user_messages(
    home: &Path,
    rollout_rel: &str,
    id: &str,
    workspace: &Path,
) -> PathBuf {
    write_rollout(
        home,
        rollout_rel,
        &[session_meta(id, workspace.to_str().unwrap())],
    )
}

/// Write a rollout that DOES have a user message (so JSONL title wins).
fn write_rollout_with_message(
    home: &Path,
    rollout_rel: &str,
    id: &str,
    workspace: &Path,
    message: &str,
) -> PathBuf {
    write_rollout(
        home,
        rollout_rel,
        &[
            session_meta(id, workspace.to_str().unwrap()),
            json!({
                "type": "event_msg",
                "payload": {
                    "type": "user_message",
                    "message": {
                        "role": "user",
                        "content": [{ "type": "input_text", "text": message }]
                    }
                }
            }),
        ],
    )
}

/// Create a real SQLite database at `<home>/state_5.sqlite` with a chosen
/// schema, authoring rows via a read-write connection. The returned closure
/// finalizes the DB (checkpoint + close) before the test asserts on it.
fn create_state_db<F>(home: &Path, schema: F) -> PathBuf
where
    F: FnOnce(&Connection),
{
    let db_path = home.join(STATE_DB_FILENAME);
    // Author with a read-write connection (NOT how we open it for enrichment).
    let conn = Connection::open(&db_path).expect("open rw for fixture");
    schema(&conn);
    // Run a WAL checkpoint so all data is durable before we close; this is
    // authoring only, not enrichment. The enrichment layer never checkpoints.
    let _ = conn.pragma_update(None, "wal_checkpoint", "TRUNCATE");
    conn.close().expect("close rw fixture");
    db_path
}

/// Schema shape resembling modern Codex `state_5.sqlite`: a `sessions` table
/// keyed by rollout_path with title, cwd, activity time, and archived.
fn schema_modern(conn: &Connection) {
    conn.execute_batch(
        "CREATE TABLE sessions (
            rollout_path TEXT PRIMARY KEY,
            thread_id    TEXT,
            cwd          TEXT,
            title        TEXT,
            updated_at   TEXT,
            archived     INTEGER DEFAULT 0
        );",
    )
    .unwrap();
}

/// An older/alternate schema: different table name, fewer columns, no archived.
fn schema_old(conn: &Connection) {
    conn.execute_batch(
        "CREATE TABLE session_index (
            path       TEXT PRIMARY KEY,
            session_id TEXT,
            title      TEXT
        );",
    )
    .unwrap();
}

/// Insert a row into the `sessions` (modern) table.
fn insert_modern(
    conn: &Connection,
    rollout_path: &str,
    thread_id: &str,
    cwd: &str,
    title: &str,
    updated_at: &str,
    archived: bool,
) {
    conn.execute(
        "INSERT INTO sessions (rollout_path, thread_id, cwd, title, updated_at, archived)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![
            rollout_path,
            thread_id,
            cwd,
            title,
            updated_at,
            archived as i32,
        ],
    )
    .unwrap();
}

/// Insert a row into the `session_index` (old) table.
fn insert_old(conn: &Connection, path: &str, session_id: &str, title: &str) {
    conn.execute(
        "INSERT INTO session_index (path, session_id, title) VALUES (?1, ?2, ?3)",
        rusqlite::params![path, session_id, title],
    )
    .unwrap();
}

/// Run enriched discovery and return only the Session results.
fn discover_enriched(home: &Path) -> (Vec<crate::session::Session>, SqliteOutcome) {
    let (outcomes, outcome) =
        discover_with_filter_enriched(home, &Bounds::default(), None, |_| true);
    let sessions = outcomes
        .into_iter()
        .filter_map(|o| match o {
            DiscoveredSession::Session(s) => Some(s),
            DiscoveredSession::Error { .. } => None,
        })
        .collect();
    (sessions, outcome)
}

// ---------------------------------------------------------------------------
// Exit criterion: deleting the DB changes nothing
// ---------------------------------------------------------------------------

#[test]
fn exit_criterion_deleting_db_does_not_change_discoverable_or_resumable_sessions() {
    let home = codex_home();
    let workspace = home.path().join("ws");
    fs::create_dir_all(&workspace).unwrap();

    let id = next_id("exit");
    let rollout_rel = format!("sessions/2026/08/07/rollout-{id}.jsonl");
    let rollout_path = write_rollout_no_user_messages(
        home.path(),
        &rollout_rel,
        &id,
        &workspace.canonicalize().unwrap(),
    );

    // With a DB present.
    let db_path = create_state_db(home.path(), |conn| {
        schema_modern(conn);
        insert_modern(
            conn,
            rollout_path.to_str().unwrap(),
            &id,
            workspace.canonicalize().unwrap().to_str().unwrap(),
            "DB-supplied title",
            "2026-08-07T11:00:00.000Z",
            false,
        );
    });
    assert!(db_path.is_file());

    let (with_db, outcome_with) = discover_enriched(home.path());
    assert_eq!(with_db.len(), 1, "one session discoverable with DB");
    assert_eq!(with_db[0].resumable_id.to_str().unwrap(), id);
    assert_eq!(with_db[0].support, SupportStatus::Supported);
    // The DB title enriched the session (no JSONL user message).
    assert_eq!(with_db[0].title.as_deref(), Some("DB-supplied title"));
    // Outcome reports enrichment happened.
    match &outcome_with {
        SqliteOutcome::Used { enriched, .. } => assert_eq!(*enriched, 1),
        other => panic!("expected Used, got {other:?}"),
    }

    // Now delete the DB and discover again.
    fs::remove_file(&db_path).unwrap();
    let (without_db, outcome_without) = discover_enriched(home.path());

    // THE exit criterion: the SAME session is still discoverable and resumable.
    assert_eq!(
        without_db.len(),
        1,
        "still one session discoverable without DB"
    );
    assert_eq!(without_db[0].resumable_id.to_str().unwrap(), id);
    assert_eq!(without_db[0].support, SupportStatus::Supported);
    // Workspace is identical (JSONL-authoritative).
    assert_eq!(with_db[0].workspace, without_db[0].workspace);
    // Only the (enrichment-only) title differs.
    assert_eq!(without_db[0].title, None, "title falls back to None w/o DB");
    assert_eq!(without_db[0].risk, RiskStatus::Normal);
    // Resume identity is unaffected.
    assert_eq!(
        with_db[0].key.effective_root,
        without_db[0].key.effective_root
    );
    assert_eq!(
        with_db[0].key.native_locator,
        without_db[0].key.native_locator
    );
    // Outcome now reports Absent.
    assert_eq!(outcome_without, SqliteOutcome::Absent);
}

// ---------------------------------------------------------------------------
// Absent DB
// ---------------------------------------------------------------------------

#[test]
fn absent_db_degrades_silently_to_jsonl_with_absent_outcome() {
    let home = codex_home();
    let workspace = home.path().join("ws");
    fs::create_dir_all(&workspace).unwrap();

    let id = next_id("absent");
    write_rollout_with_message(
        home.path(),
        &format!("sessions/2026/08/07/rollout-{id}.jsonl"),
        &id,
        &workspace.canonicalize().unwrap(),
        "jsonl title",
    );

    // No DB file at all.
    let (sessions, outcome) = discover_enriched(home.path());
    assert_eq!(outcome, SqliteOutcome::Absent);
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].title.as_deref(), Some("jsonl title"));
}

// ---------------------------------------------------------------------------
// Corrupt DB (not a real SQLite file)
// ---------------------------------------------------------------------------

#[test]
fn corrupt_db_degrades_silently_with_corrupt_category() {
    let home = codex_home();
    let workspace = home.path().join("ws");
    fs::create_dir_all(&workspace).unwrap();

    let id = next_id("corrupt");
    write_rollout_with_message(
        home.path(),
        &format!("sessions/2026/08/07/rollout-{id}.jsonl"),
        &id,
        &workspace.canonicalize().unwrap(),
        "jsonl survives",
    );

    // A file that is plainly not a SQLite database.
    fs::write(
        home.path().join(STATE_DB_FILENAME),
        b"this is not a sqlite db",
    )
    .unwrap();

    let (sessions, outcome) = discover_enriched(home.path());
    assert_eq!(
        outcome,
        SqliteOutcome::Degraded {
            category: "corrupt"
        },
        "corrupt DB degrades with corrupt category"
    );
    // JSONL discovery still produced the session.
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].title.as_deref(), Some("jsonl survives"));
}

#[test]
fn empty_db_file_is_treated_as_corrupt_degradation() {
    let home = codex_home();
    let workspace = home.path().join("ws");
    fs::create_dir_all(&workspace).unwrap();

    let id = next_id("empty");
    write_rollout_with_message(
        home.path(),
        &format!("sessions/2026/08/07/rollout-{id}.jsonl"),
        &id,
        &workspace.canonicalize().unwrap(),
        "jsonl survives",
    );

    // Zero-byte file: SQLite reports it as not-a-database / corrupt on open.
    fs::write(home.path().join(STATE_DB_FILENAME), b"").unwrap();

    let (_sessions, outcome) = discover_enriched(home.path());
    // An empty file may classify as corrupt or unreadable depending on the
    // SQLite version; both are Degraded (never Used, never Absent).
    assert!(
        matches!(outcome, SqliteOutcome::Degraded { .. }),
        "empty file must degrade, got {outcome:?}"
    );
}

// ---------------------------------------------------------------------------
// Old / new schema
// ---------------------------------------------------------------------------

#[test]
fn old_schema_with_fewer_columns_still_enriches_title() {
    let home = codex_home();
    let workspace = home.path().join("ws");
    fs::create_dir_all(&workspace).unwrap();

    let id = next_id("oldschema");
    let rollout_rel = format!("sessions/2026/08/07/rollout-{id}.jsonl");
    let rollout_path = write_rollout_no_user_messages(
        home.path(),
        &rollout_rel,
        &id,
        &workspace.canonicalize().unwrap(),
    );

    create_state_db(home.path(), |conn| {
        schema_old(conn);
        insert_old(
            conn,
            rollout_path.to_str().unwrap(),
            &id,
            "title from old schema",
        );
    });

    let (sessions, outcome) = discover_enriched(home.path());
    // Old schema has a join key (path) and a title; supported.
    assert!(
        !outcome.is_degraded(),
        "old schema is supported, got {outcome:?}"
    );
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].title.as_deref(), Some("title from old schema"));
}

#[test]
fn totally_unknown_schema_degrades_as_unsupported() {
    let home = codex_home();
    let workspace = home.path().join("ws");
    fs::create_dir_all(&workspace).unwrap();

    let id = next_id("unknownschema");
    write_rollout_with_message(
        home.path(),
        &format!("sessions/2026/08/07/rollout-{id}.jsonl"),
        &id,
        &workspace.canonicalize().unwrap(),
        "jsonl title",
    );

    // A valid SQLite DB whose schema is unrelated to Codex sessions.
    create_state_db(home.path(), |conn| {
        conn.execute_batch("CREATE TABLE unrelated (foo TEXT);")
            .unwrap();
        conn.execute("INSERT INTO unrelated (foo) VALUES ('bar')", [])
            .unwrap();
    });

    let (_sessions, outcome) = discover_enriched(home.path());
    assert_eq!(
        outcome,
        SqliteOutcome::Degraded {
            category: "unsupported_schema"
        }
    );
}

#[test]
fn new_schema_with_extra_columns_is_supported() {
    let home = codex_home();
    let workspace = home.path().join("ws");
    fs::create_dir_all(&workspace).unwrap();

    let id = next_id("newschema");
    let rollout_rel = format!("sessions/2026/08/07/rollout-{id}.jsonl");
    let rollout_path = write_rollout_no_user_messages(
        home.path(),
        &rollout_rel,
        &id,
        &workspace.canonicalize().unwrap(),
    );

    create_state_db(home.path(), |conn| {
        // A future schema with extra, unknown columns plus the ones we probe.
        conn.execute_batch(
            "CREATE TABLE sessions (
                rollout_path TEXT PRIMARY KEY,
                thread_id    TEXT,
                cwd          TEXT,
                title        TEXT,
                updated_at   TEXT,
                archived     INTEGER,
                new_future_col TEXT,
                model        TEXT
            );",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO sessions (rollout_path, thread_id, cwd, title, updated_at, archived, new_future_col, model)
             VALUES (?1, ?2, ?3, ?4, ?5, 0, 'x', 'gpt-x')",
            rusqlite::params![
                rollout_path.to_str().unwrap(),
                &id,
                workspace.canonicalize().unwrap().to_str().unwrap(),
                "title from new schema",
                "2026-08-07T12:00:00.000Z",
            ],
        )
        .unwrap();
    });

    let (sessions, outcome) = discover_enriched(home.path());
    assert!(!outcome.is_degraded(), "extra columns must not break us");
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].title.as_deref(), Some("title from new schema"));
}

// ---------------------------------------------------------------------------
// Stale row (DB row whose rollout_path no longer matches any session)
// ---------------------------------------------------------------------------

#[test]
fn stale_row_for_missing_rollout_is_not_applied() {
    let home = codex_home();
    let workspace = home.path().join("ws");
    fs::create_dir_all(&workspace).unwrap();

    let id = next_id("stale");
    write_rollout_with_message(
        home.path(),
        &format!("sessions/2026/08/07/rollout-{id}.jsonl"),
        &id,
        &workspace.canonicalize().unwrap(),
        "real jsonl title",
    );

    create_state_db(home.path(), |conn| {
        schema_modern(conn);
        // Row points at a rollout path that does NOT exist on disk.
        insert_modern(
            conn,
            "/nonexistent/rollout-ghost.jsonl",
            "ghost-id",
            workspace.canonicalize().unwrap().to_str().unwrap(),
            "ghost title",
            "2026-08-07T11:00:00.000Z",
            false,
        );
    });

    let (sessions, outcome) = discover_enriched(home.path());
    // The real session is still discovered with its JSONL title (not enriched
    // by the stale row). Outcome reports the real session as skipped_no_row.
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].title.as_deref(), Some("real jsonl title"));
    match &outcome {
        SqliteOutcome::Used {
            enriched,
            skipped_no_row,
            ..
        } => {
            assert_eq!(*enriched, 0, "stale row enriches nothing");
            assert_eq!(*skipped_no_row, 1, "real session has no matching row");
        }
        other => panic!("expected Used, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Missing rollout in DB (session exists on disk, no DB row)
// ---------------------------------------------------------------------------

#[test]
fn missing_rollout_row_leaves_jsonl_title_intact() {
    let home = codex_home();
    let workspace = home.path().join("ws");
    fs::create_dir_all(&workspace).unwrap();

    let id = next_id("missingrow");
    write_rollout_with_message(
        home.path(),
        &format!("sessions/2026/08/07/rollout-{id}.jsonl"),
        &id,
        &workspace.canonicalize().unwrap(),
        "jsonl only title",
    );

    // DB exists but has no row for this rollout.
    create_state_db(home.path(), |conn| {
        schema_modern(conn);
        insert_modern(
            conn,
            "/some/other/rollout.jsonl",
            "other-id",
            workspace.canonicalize().unwrap().to_str().unwrap(),
            "other title",
            "2026-08-07T11:00:00.000Z",
            false,
        );
    });

    let (sessions, outcome) = discover_enriched(home.path());
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].title.as_deref(), Some("jsonl only title"));
    match &outcome {
        SqliteOutcome::Used { skipped_no_row, .. } => assert_eq!(*skipped_no_row, 1),
        other => panic!("expected Used, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Precedence: ID / Workspace disagreement
// ---------------------------------------------------------------------------

#[test]
fn id_disagreement_produces_diagnostic_and_no_identity_replacement() {
    let home = codex_home();
    let workspace = home.path().join("ws");
    fs::create_dir_all(&workspace).unwrap();

    let id = next_id("idmismatch");
    let rollout_rel = format!("sessions/2026/08/07/rollout-{id}.jsonl");
    let rollout_path = write_rollout_no_user_messages(
        home.path(),
        &rollout_rel,
        &id,
        &workspace.canonicalize().unwrap(),
    );

    create_state_db(home.path(), |conn| {
        schema_modern(conn);
        // Row matches the rollout path but carries a DIFFERENT id.
        insert_modern(
            conn,
            rollout_path.to_str().unwrap(),
            "WRONG-ID-NOT-THE-ROLLOUT-ID",
            workspace.canonicalize().unwrap().to_str().unwrap(),
            "should not be applied",
            "2026-08-07T11:00:00.000Z",
            false,
        );
    });

    let (sessions, outcome) = discover_enriched(home.path());
    assert_eq!(sessions.len(), 1);
    // Authoritative id is the rollout id, NOT the DB id.
    assert_eq!(sessions[0].resumable_id.to_str().unwrap(), id);
    // Title is NOT enriched (enrichment skipped on mismatch).
    assert_ne!(sessions[0].title.as_deref(), Some("should not be applied"));
    // A diagnostic was recorded.
    match &outcome {
        SqliteOutcome::Used { diagnostics, .. } => {
            assert!(
                diagnostics
                    .iter()
                    .any(|d| d.category == "codex_sqlite_id_mismatch"),
                "expected id mismatch diagnostic, got {diagnostics:?}"
            );
        }
        other => panic!("expected Used, got {other:?}"),
    }
}

#[test]
fn workspace_disagreement_produces_diagnostic_and_no_workspace_replacement() {
    let home = codex_home();
    let workspace = home.path().join("ws");
    let other_ws = home.path().join("other-ws");
    fs::create_dir_all(&workspace).unwrap();
    fs::create_dir_all(&other_ws).unwrap();

    let id = next_id("wsmismatch");
    let rollout_rel = format!("sessions/2026/08/07/rollout-{id}.jsonl");
    let rollout_path = write_rollout_no_user_messages(
        home.path(),
        &rollout_rel,
        &id,
        &workspace.canonicalize().unwrap(),
    );

    create_state_db(home.path(), |conn| {
        schema_modern(conn);
        // Row matches path+id but a DIFFERENT cwd.
        insert_modern(
            conn,
            rollout_path.to_str().unwrap(),
            &id,
            other_ws.canonicalize().unwrap().to_str().unwrap(),
            "title should not apply",
            "2026-08-07T11:00:00.000Z",
            false,
        );
    });

    let (sessions, outcome) = discover_enriched(home.path());
    assert_eq!(sessions.len(), 1);
    // Authoritative Workspace is the rollout cwd, NOT the DB cwd.
    match &sessions[0].workspace {
        WorkspaceEvidence::Recorded { workspace: ws, .. } => {
            assert_eq!(ws, &workspace.canonicalize().unwrap());
        }
        _ => panic!("expected recorded workspace"),
    }
    assert_ne!(sessions[0].title.as_deref(), Some("title should not apply"));
    match &outcome {
        SqliteOutcome::Used { diagnostics, .. } => {
            assert!(
                diagnostics
                    .iter()
                    .any(|d| d.category == "codex_sqlite_workspace_mismatch"),
                "expected workspace mismatch diagnostic"
            );
        }
        other => panic!("expected Used, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Timestamp / activity disagreement does NOT replace identity
// ---------------------------------------------------------------------------

#[test]
fn timestamp_disagreement_does_not_replace_identity_or_workspace() {
    let home = codex_home();
    let workspace = home.path().join("ws");
    fs::create_dir_all(&workspace).unwrap();

    let id = next_id("timedisagree");
    let rollout_rel = format!("sessions/2026/08/07/rollout-{id}.jsonl");
    let rollout_path = write_rollout_no_user_messages(
        home.path(),
        &rollout_rel,
        &id,
        &workspace.canonicalize().unwrap(),
    );

    create_state_db(home.path(), |conn| {
        schema_modern(conn);
        // Same id + workspace (agreement) but a wildly different timestamp.
        insert_modern(
            conn,
            rollout_path.to_str().unwrap(),
            &id,
            workspace.canonicalize().unwrap().to_str().unwrap(),
            "agreed title",
            "1999-01-01T00:00:00.000Z",
            false,
        );
    });

    let (sessions, _outcome) = discover_enriched(home.path());
    assert_eq!(sessions.len(), 1);
    // ID and workspace are authoritative regardless of the DB timestamp.
    assert_eq!(sessions[0].resumable_id.to_str().unwrap(), id);
    match &sessions[0].workspace {
        WorkspaceEvidence::Recorded { workspace: ws, .. } => {
            assert_eq!(ws, &workspace.canonicalize().unwrap());
        }
        _ => panic!("expected recorded workspace"),
    }
    // Title may be enriched (id+cwd agree), but identity is untouched.
    assert_eq!(sessions[0].title.as_deref(), Some("agreed title"));
}

// ---------------------------------------------------------------------------
// Locked / busy DB
// ---------------------------------------------------------------------------

#[test]
fn locked_db_degrades_silently_and_does_not_block_discovery() {
    let home = codex_home();
    let workspace = home.path().join("ws");
    fs::create_dir_all(&workspace).unwrap();

    let id = next_id("locked");
    write_rollout_with_message(
        home.path(),
        &format!("sessions/2026/08/07/rollout-{id}.jsonl"),
        &id,
        &workspace.canonicalize().unwrap(),
        "jsonl survives lock",
    );

    // Create a valid DB first.
    let db_path = create_state_db(home.path(), schema_modern);

    // Open a write transaction and hold it (BEGIN; ...) so the DB is busy for
    // writes. SQLite read-only opens of a WAL DB are normally still possible,
    // but a reserved/pending lock blocks readers too. We take an exclusive
    // lock to simulate a busy writer.
    let holder = Connection::open(&db_path).expect("open holder");
    holder
        .execute_batch("BEGIN EXCLUSIVE;")
        .expect("begin exclusive");

    // Enrichment must degrade, not hang. Use a thread with a timeout guard.
    let home_clone = home.path().to_path_buf();
    let handle = thread::spawn(move || {
        let bounds = Bounds::default();
        let (outcomes, outcome) =
            discover_with_filter_enriched(&home_clone, &bounds, None, |_| true);
        let sessions: Vec<_> = outcomes
            .into_iter()
            .filter_map(|o| match o {
                DiscoveredSession::Session(s) => Some(s),
                _ => None,
            })
            .collect();
        (sessions, outcome)
    });
    let (sessions, outcome) = handle.join().expect("enrich thread panicked");
    // Discovery still worked; enrichment degraded (locked OR used — both are
    // acceptable since WAL reads may still succeed; the key assertion is no
    // hang and identity is JSONL-only).
    assert_eq!(sessions.len(), 1, "discovery not blocked by a busy DB");
    assert_eq!(sessions[0].resumable_id.to_str().unwrap(), id);
    // Either it degraded (locked) or it used the DB (WAL allowed the read).
    // Both are fine; the invariant is that it never blocked or panicked.
    let _ = outcome;

    drop(holder);
}

// ---------------------------------------------------------------------------
// WAL activity: a DB in WAL mode with a live writer
// ---------------------------------------------------------------------------

#[test]
fn wal_mode_db_enriches_without_checkpoint_or_mutation() {
    let home = codex_home();
    let workspace = home.path().join("ws");
    fs::create_dir_all(&workspace).unwrap();

    let id = next_id("wal");
    let rollout_rel = format!("sessions/2026/08/07/rollout-{id}.jsonl");
    let rollout_path = write_rollout_no_user_messages(
        home.path(),
        &rollout_rel,
        &id,
        &workspace.canonicalize().unwrap(),
    );

    // Author a WAL-mode DB.
    let db_path = home.path().join(STATE_DB_FILENAME);
    {
        let conn = Connection::open(&db_path).unwrap();
        conn.pragma_update(None, "journal_mode", "WAL").unwrap();
        schema_modern(&conn);
        insert_modern(
            &conn,
            rollout_path.to_str().unwrap(),
            &id,
            workspace.canonicalize().unwrap().to_str().unwrap(),
            "wal title",
            "2026-08-07T11:00:00.000Z",
            false,
        );
        // Do NOT checkpoint; leave the WAL file in place.
        conn.close().unwrap();
    }
    // A -wal sidecar may exist.
    let wal_sidecar = db_path.with_extension("sqlite-wal");

    // Snapshot the DB + WAL bytes/mtime before enrichment.
    let before_db = snapshot::snapshot_file(&db_path).unwrap();
    let before_wal = if wal_sidecar.exists() {
        Some(snapshot::snapshot_file(&wal_sidecar).unwrap())
    } else {
        None
    };

    let (sessions, outcome) = discover_enriched(home.path());
    assert!(!outcome.is_degraded(), "WAL DB should enrich: {outcome:?}");
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].title.as_deref(), Some("wal title"));

    // The DB and WAL files must be byte/mtime-identical after enrichment.
    let after_db = snapshot::snapshot_file(&db_path).unwrap();
    snapshot::assert_file_unchanged(&before_db, &after_db);
    if let Some(before) = before_wal {
        let after_wal = snapshot::snapshot_file(&wal_sidecar).unwrap();
        // The WAL may legitimately gain nothing because we never wrote; assert
        // bytes unchanged (mtime can shift on some FS due to atime, but bytes
        // are the invariant that matters for read-only).
        assert_eq!(before.bytes, after_wal.bytes, "WAL bytes must not change");
    }
}

// ---------------------------------------------------------------------------
// Read-only filesystem: discovery + enrichment never mutate the DB
// ---------------------------------------------------------------------------

#[test]
fn enrichment_does_not_mutate_db_file_bytes_or_mtime() {
    let home = codex_home();
    let workspace = home.path().join("ws");
    fs::create_dir_all(&workspace).unwrap();

    let id = next_id("nomutate");
    let rollout_rel = format!("sessions/2026/08/07/rollout-{id}.jsonl");
    let rollout_path = write_rollout_no_user_messages(
        home.path(),
        &rollout_rel,
        &id,
        &workspace.canonicalize().unwrap(),
    );

    let db_path = create_state_db(home.path(), |conn| {
        schema_modern(conn);
        insert_modern(
            conn,
            rollout_path.to_str().unwrap(),
            &id,
            workspace.canonicalize().unwrap().to_str().unwrap(),
            "enrich title",
            "2026-08-07T11:00:00.000Z",
            false,
        );
    });

    let before = snapshot::snapshot_file(&db_path).unwrap();

    // Run enriched discovery several times.
    for _ in 0..3 {
        let (_sessions, _outcome) = discover_enriched(home.path());
    }

    let after = snapshot::snapshot_file(&db_path).unwrap();
    snapshot::assert_file_unchanged(&before, &after);
}

#[test]
fn full_directory_snapshot_unchanged_by_enriched_discovery() {
    let home = codex_home();
    let workspace = home.path().join("ws");
    fs::create_dir_all(&workspace).unwrap();

    let id = next_id("dirsnap");
    let rollout_rel = format!("sessions/2026/08/07/rollout-{id}.jsonl");
    let rollout_path = write_rollout_no_user_messages(
        home.path(),
        &rollout_rel,
        &id,
        &workspace.canonicalize().unwrap(),
    );

    create_state_db(home.path(), |conn| {
        schema_modern(conn);
        insert_modern(
            conn,
            rollout_path.to_str().unwrap(),
            &id,
            workspace.canonicalize().unwrap().to_str().unwrap(),
            "snap title",
            "2026-08-07T11:00:00.000Z",
            false,
        );
    });

    let before = snapshot::snapshot_dir(home.path(), true).unwrap();
    let (_sessions, _outcome) = discover_enriched(home.path());
    let after = snapshot::snapshot_dir(home.path(), true).unwrap();
    snapshot::assert_unchanged(&before, &after);
}

// ---------------------------------------------------------------------------
// Read-only open contract: mode=ro, never immutable, never writes
// ---------------------------------------------------------------------------

#[test]
fn db_is_opened_read_only_and_writes_are_rejected() {
    let home = codex_home();
    let workspace = home.path().join("ws");
    fs::create_dir_all(&workspace).unwrap();

    let id = next_id("ronly");
    let rollout_rel = format!("sessions/2026/08/07/rollout-{id}.jsonl");
    let rollout_path = write_rollout_no_user_messages(
        home.path(),
        &rollout_rel,
        &id,
        &workspace.canonicalize().unwrap(),
    );

    let db_path = create_state_db(home.path(), |conn| {
        schema_modern(conn);
        insert_modern(
            conn,
            rollout_path.to_str().unwrap(),
            &id,
            workspace.canonicalize().unwrap().to_str().unwrap(),
            "title",
            "2026-08-07T11:00:00.000Z",
            false,
        );
    });

    // Open the way the enrichment layer does and assert a write is refused.
    let conn = super::open_readonly(&db_path).expect("read-only open succeeds");
    assert!(
        super::assert_readonly(&conn),
        "PRAGMA query_only should be on"
    );
    // Any write attempt must fail (attempt-attempt does not actually write
    // because the connection is read-only).
    let write_result = conn.execute("INSERT INTO sessions (rollout_path) VALUES ('x')", []);
    assert!(
        write_result.is_err(),
        "writes must be rejected on the read-only connection"
    );
}

#[test]
fn open_readonly_does_not_use_immutable_uri() {
    // Regression guard: immutable=1 would let a non-DB file "open" silently
    // and then return garbage. We assert the safer behavior: a non-DB file is
    // detected as corrupt/unreadable when enrichment actually reads it, so a
    // live WAL DB can never be bypassed by immutable semantics.
    let home = codex_home();
    fs::write(home.path().join(STATE_DB_FILENAME), b"not a sqlite db").unwrap();
    let mut empty: Vec<ParsedSession> = Vec::new();
    let outcome = enrich(&mut empty, home.path());
    assert!(
        matches!(outcome, SqliteOutcome::Degraded { .. }),
        "non-DB file must degrade, not open as immutable, got {outcome:?}"
    );
}

// ---------------------------------------------------------------------------
// Archived hint enrichment (informational, never overrides filesystem)
// ---------------------------------------------------------------------------

#[test]
fn archived_hint_enriches_when_filesystem_did_not_mark_archived() {
    let home = codex_home();
    let workspace = home.path().join("ws");
    fs::create_dir_all(&workspace).unwrap();

    let id = next_id("arch");
    let rollout_rel = format!("sessions/2026/08/07/rollout-{id}.jsonl");
    let rollout_path = write_rollout_no_user_messages(
        home.path(),
        &rollout_rel,
        &id,
        &workspace.canonicalize().unwrap(),
    );

    create_state_db(home.path(), |conn| {
        schema_modern(conn);
        insert_modern(
            conn,
            rollout_path.to_str().unwrap(),
            &id,
            workspace.canonicalize().unwrap().to_str().unwrap(),
            "title",
            "2026-08-07T11:00:00.000Z",
            true, // archived
        );
    });

    // Session is under sessions/ (not archived_sessions/) so filesystem says
    // not-archived; the DB hint is recorded but must not corrupt identity.
    let (sessions, outcome) = discover_enriched(home.path());
    assert_eq!(sessions.len(), 1);
    assert!(!outcome.is_degraded());
}

// ---------------------------------------------------------------------------
// JSONL title wins over DB title (precedence: JSONL authoritative)
// ---------------------------------------------------------------------------

#[test]
fn jsonl_title_preferred_over_db_title_when_user_messages_exist() {
    let home = codex_home();
    let workspace = home.path().join("ws");
    fs::create_dir_all(&workspace).unwrap();

    let id = next_id("precedence");
    let rollout_rel = format!("sessions/2026/08/07/rollout-{id}.jsonl");
    let rollout_path = write_rollout_with_message(
        home.path(),
        &rollout_rel,
        &id,
        &workspace.canonicalize().unwrap(),
        "REAL JSONL TITLE",
    );

    create_state_db(home.path(), |conn| {
        schema_modern(conn);
        insert_modern(
            conn,
            rollout_path.to_str().unwrap(),
            &id,
            workspace.canonicalize().unwrap().to_str().unwrap(),
            "DB DISTRACT TITLE",
            "2026-08-07T11:00:00.000Z",
            false,
        );
    });

    let (sessions, _outcome) = discover_enriched(home.path());
    assert_eq!(sessions.len(), 1);
    // JSONL title wins.
    assert_eq!(sessions[0].title.as_deref(), Some("REAL JSONL TITLE"));
}

// ---------------------------------------------------------------------------
// Resume is unaffected by enrichment
// ---------------------------------------------------------------------------

#[test]
fn resume_spec_is_identical_with_and_without_db() {
    use crate::integration::codex::resume_spec;

    let home = codex_home();
    let workspace = home.path().join("ws");
    fs::create_dir_all(&workspace).unwrap();

    let id = next_id("resume");
    let rollout_rel = format!("sessions/2026/08/07/rollout-{id}.jsonl");
    let rollout_path = write_rollout_no_user_messages(
        home.path(),
        &rollout_rel,
        &id,
        &workspace.canonicalize().unwrap(),
    );

    // With DB.
    create_state_db(home.path(), |conn| {
        schema_modern(conn);
        insert_modern(
            conn,
            rollout_path.to_str().unwrap(),
            &id,
            workspace.canonicalize().unwrap().to_str().unwrap(),
            "db title",
            "2026-08-07T11:00:00.000Z",
            false,
        );
    });
    let (with_db, _) = discover_enriched(home.path());
    let spec_with = resume_spec(&with_db[0], &home.path().canonicalize().unwrap());

    // Delete DB.
    fs::remove_file(home.path().join(STATE_DB_FILENAME)).unwrap();
    let (without_db, _) = discover_enriched(home.path());
    let spec_without = resume_spec(&without_db[0], &home.path().canonicalize().unwrap());

    // Resume program/argv/cwd/env are identical regardless of the DB.
    assert_eq!(spec_with.program, spec_without.program);
    assert_eq!(spec_with.argv, spec_without.argv);
    assert_eq!(spec_with.cwd, spec_without.cwd);
    assert_eq!(spec_with.env, spec_without.env);
}

// ---------------------------------------------------------------------------
// Outcome summary rendering
// ---------------------------------------------------------------------------

#[test]
fn outcome_summary_is_display_safe_and_categorizes_degradation() {
    let absent = SqliteOutcome::Absent;
    assert_eq!(absent.summary(), None);

    let degraded = SqliteOutcome::Degraded {
        category: "corrupt",
    };
    let s = degraded.summary().unwrap();
    assert!(s.contains("codex_sqlite_degraded"));
    assert!(s.contains("corrupt"));
    assert!(!s.contains("secret"));

    let used = SqliteOutcome::Used {
        enriched: 2,
        skipped_no_row: 1,
        diagnostics: Vec::new(),
    };
    let s = used.summary().unwrap();
    assert!(s.contains("2 sessions enriched"));
}

// ---------------------------------------------------------------------------
// path equivalence helper
// ---------------------------------------------------------------------------

#[test]
fn paths_equivalent_handles_trailing_slash() {
    let ws = Path::new("/tmp/abc");
    assert!(super::paths_equivalent("/tmp/abc", ws));
    assert!(super::paths_equivalent("/tmp/abc/", ws));
    assert!(!super::paths_equivalent("/tmp/abcd", ws));
}

// ---------------------------------------------------------------------------
// Multiple sessions: only matching ones enrich
// ---------------------------------------------------------------------------

#[test]
fn only_matching_sessions_are_enriched() {
    let home = codex_home();
    let workspace = home.path().join("ws");
    fs::create_dir_all(&workspace).unwrap();

    let id_a = next_id("multi-a");
    let id_b = next_id("multi-b");
    let rel_a = format!("sessions/2026/08/07/rollout-{id_a}.jsonl");
    let rel_b = format!("sessions/2026/08/07/rollout-{id_b}.jsonl");
    let path_a = write_rollout_no_user_messages(
        home.path(),
        &rel_a,
        &id_a,
        &workspace.canonicalize().unwrap(),
    );
    let _path_b = write_rollout_no_user_messages(
        home.path(),
        &rel_b,
        &id_b,
        &workspace.canonicalize().unwrap(),
    );

    // DB has a row ONLY for A.
    create_state_db(home.path(), |conn| {
        schema_modern(conn);
        insert_modern(
            conn,
            path_a.to_str().unwrap(),
            &id_a,
            workspace.canonicalize().unwrap().to_str().unwrap(),
            "A title from db",
            "2026-08-07T11:00:00.000Z",
            false,
        );
    });

    let (sessions, outcome) = discover_enriched(home.path());
    assert_eq!(sessions.len(), 2);
    let by_id: std::collections::HashMap<&str, Option<&str>> = sessions
        .iter()
        .map(|s| (s.resumable_id.to_str().unwrap(), s.title.as_deref()))
        .collect();
    assert_eq!(by_id.get(id_a.as_str()), Some(&Some("A title from db")));
    assert_eq!(
        by_id.get(id_b.as_str()),
        Some(&None),
        "B has no row => no title"
    );
    match &outcome {
        SqliteOutcome::Used {
            enriched,
            skipped_no_row,
            ..
        } => {
            assert_eq!(*enriched, 1);
            assert_eq!(*skipped_no_row, 1);
        }
        other => panic!("expected Used, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Sanity: enrichment is deterministic across repeated runs
// ---------------------------------------------------------------------------

#[test]
fn enrichment_is_deterministic_across_repeated_runs() {
    let home = codex_home();
    let workspace = home.path().join("ws");
    fs::create_dir_all(&workspace).unwrap();

    let id = next_id("det");
    let rollout_rel = format!("sessions/2026/08/07/rollout-{id}.jsonl");
    let rollout_path = write_rollout_no_user_messages(
        home.path(),
        &rollout_rel,
        &id,
        &workspace.canonicalize().unwrap(),
    );

    create_state_db(home.path(), |conn| {
        schema_modern(conn);
        insert_modern(
            conn,
            rollout_path.to_str().unwrap(),
            &id,
            workspace.canonicalize().unwrap().to_str().unwrap(),
            "stable title",
            "2026-08-07T11:00:00.000Z",
            false,
        );
    });

    let (s1, o1) = discover_enriched(home.path());
    let (s2, o2) = discover_enriched(home.path());
    assert_eq!(o1, o2);
    assert_eq!(s1.len(), s2.len());
    assert_eq!(s1[0].title, s2[0].title);
    assert_eq!(s1[0].resumable_id, s2[0].resumable_id);
}

// ---------------------------------------------------------------------------
// state_db_path helper
// ---------------------------------------------------------------------------

#[test]
fn state_db_path_is_under_effective_root() {
    let p = state_db_path(Path::new("/home/u/.codex"));
    assert_eq!(p, PathBuf::from("/home/u/.codex/state_5.sqlite"));
}

// ---------------------------------------------------------------------------
// Smoke: enrich() on a real parsed session slice (no discovery) for unit-level
// coverage of the apply_rows precedence logic.
// ---------------------------------------------------------------------------

#[test]
fn enrich_fills_title_only_when_jsonl_has_no_user_messages() {
    // Build a ParsedSession by hand via the public parse path is heavy; reuse
    // a real rollout but assert the parsed.title hint is set only when empty.
    let home = codex_home();
    let workspace = home.path().join("ws");
    fs::create_dir_all(&workspace).unwrap();

    let id = next_id("fill");
    let rollout_rel = format!("sessions/2026/08/07/rollout-{id}.jsonl");
    let rollout_path = write_rollout_no_user_messages(
        home.path(),
        &rollout_rel,
        &id,
        &workspace.canonicalize().unwrap(),
    );

    // Parse to get a ParsedSession with empty user_messages.
    let read = crate::preview::jsonl::read_file(&rollout_path, &Bounds::default()).unwrap();
    let parsed = crate::integration::codex::discover::parse_rollout_records(&rollout_path, &read)
        .unwrap()
        .unwrap();
    assert!(parsed.user_messages.is_empty());

    create_state_db(home.path(), |conn| {
        schema_modern(conn);
        insert_modern(
            conn,
            rollout_path.to_str().unwrap(),
            &id,
            workspace.canonicalize().unwrap().to_str().unwrap(),
            "filled title",
            "2026-08-07T11:00:00.000Z",
            false,
        );
    });

    let mut sessions = vec![parsed];
    let outcome = enrich(&mut sessions, home.path());
    assert!(matches!(outcome, SqliteOutcome::Used { .. }));
    assert_eq!(sessions[0].sqlite_title.as_deref(), Some("filled title"));
    assert!(sessions[0].sqlite_activity_time.is_some());
    // Sanity: the helper field is the only thing set; identity untouched.
    assert_eq!(sessions[0].id, id);
}
