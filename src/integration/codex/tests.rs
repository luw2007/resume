//! Tests for the Codex JSONL integration.
//!
//! Covers the full Step 6 matrix: modern/older/archived/imported/noninteractive
//! shapes, ID/filename mismatch, alternate root, duplicate representation,
//! unknown records, missing Workspace, malformed/truncated files, and the fake
//! exact argv/env for `codex`. Read-only verification uses the shared snapshot
//! helpers.

#![cfg(test)]

use std::{
    fs,
    path::{Path, PathBuf},
};

use serde_json::json;

use super::roots::{dirs_home, is_rollout_filename};
use super::*;
use crate::{
    preview::jsonl::Bounds,
    preview::snapshot,
    session::{
        ActivityStatus, IntegrationError, RiskStatus, Session, SupportStatus, WorkspaceEvidence,
    },
};

#[test]
fn public_extract_user_messages_api_is_preserved() {
    let extract: fn(&[serde_json::Value]) -> Vec<crate::preview::message::UserMessage> =
        super::extract_user_messages;
    assert!(extract(&[]).is_empty());
}

// ---------------------------------------------------------------------------
// Fixture helpers
// ---------------------------------------------------------------------------

/// Build a Codex-style home directory under a temp dir and return its path.
fn codex_home() -> tempfile::TempDir {
    tempfile::tempdir().expect("temp dir")
}

/// Write a rollout file with the given JSON records (one per line). Returns
/// the absolute path. Dated subdirs are created as needed from `rel`.
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

/// A canonical modern session_meta record.
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
            "model_provider": "openai"
        }
    })
}

/// An older session_meta shape (fewer payload fields, no model_provider).
fn older_session_meta(id: &str, cwd: &str) -> serde_json::Value {
    json!({
        "timestamp": "2025-01-01T00:00:00.000Z",
        "type": "session_meta",
        "payload": {
            "id": id,
            "cwd": cwd,
            "originator": "cli",
            "cli_version": "0.20.0"
        }
    })
}

/// An event_msg user_message record.
fn event_msg_user(text: &str) -> serde_json::Value {
    json!({
        "timestamp": "2026-08-07T10:00:01.000Z",
        "type": "event_msg",
        "payload": {
            "type": "user_message",
            "message": {
                "role": "user",
                "content": [{ "type": "input_text", "text": text }]
            }
        }
    })
}

/// A response_item user message record (the second representation).
fn response_item_user(text: &str) -> serde_json::Value {
    json!({
        "timestamp": "2026-08-07T10:00:01.000Z",
        "type": "response_item",
        "payload": {
            "type": "message",
            "message": {
                "role": "user",
                "content": [{ "type": "input_text", "text": text }]
            }
        }
    })
}

/// An assistant response_item (must be excluded).
fn response_item_assistant(text: &str) -> serde_json::Value {
    json!({
        "type": "response_item",
        "payload": {
            "type": "message",
            "message": {
                "role": "assistant",
                "content": [{ "type": "output_text", "text": text }]
            }
        }
    })
}

/// A developer-injected response_item (must be excluded).
fn response_item_developer(text: &str) -> serde_json::Value {
    json!({
        "type": "response_item",
        "payload": {
            "type": "message",
            "message": {
                "role": "developer",
                "content": [{ "type": "input_text", "text": text }]
            }
        }
    })
}

/// Discover sessions in a fresh effective root, returning only the Session
/// results (errors filtered for convenience).
fn discover_sessions(home: &Path) -> Vec<Session> {
    let outcomes = discover(home, &Bounds::default());
    outcomes
        .into_iter()
        .filter_map(|o| match o {
            DiscoveredSession::Session(s) => Some(s),
            DiscoveredSession::Error { .. } => None,
        })
        .collect()
}

/// Like [`discover_sessions`] but surfaces errors too.
fn discover_outcomes(home: &Path) -> Vec<DiscoveredSession> {
    discover(home, &Bounds::default())
}

// ---------------------------------------------------------------------------
// Storage shape and root resolution
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn symlinked_rollout_inside_effective_root_is_read() {
    let home = codex_home();
    let workspace = home.path().join("ws");
    fs::create_dir_all(&workspace).unwrap();
    let target = write_rollout(
        home.path(),
        "inside-target.data",
        &[
            session_meta(
                "inside-link",
                workspace.canonicalize().unwrap().to_str().unwrap(),
            ),
            event_msg_user("followed safely"),
        ],
    );
    let link = home.path().join("sessions/2026/08/07/rollout-inside.jsonl");
    fs::create_dir_all(link.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&target, &link).unwrap();

    let outcomes = discover_outcomes(home.path());
    let sessions: Vec<_> = outcomes
        .iter()
        .filter_map(DiscoveredSession::session)
        .collect();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].resumable_id.to_str(), Some("inside-link"));
    assert_eq!(sessions[0].title.as_deref(), Some("followed safely"));
    assert!(
        !outcomes
            .iter()
            .any(|outcome| matches!(outcome, DiscoveredSession::Error { .. }))
    );
}

#[cfg(unix)]
#[test]
fn symlinked_rollout_outside_effective_root_is_rejected_with_error() {
    let home = codex_home();
    let workspace = home.path().join("ws");
    fs::create_dir_all(&workspace).unwrap();
    let outside = tempfile::tempdir().unwrap();
    let target = write_rollout(
        outside.path(),
        "foreign.data",
        &[
            session_meta(
                "outside-link",
                workspace.canonicalize().unwrap().to_str().unwrap(),
            ),
            event_msg_user("must not leak"),
        ],
    );
    let link = home
        .path()
        .join("sessions/2026/08/07/rollout-outside.jsonl");
    fs::create_dir_all(link.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&target, &link).unwrap();

    let outcomes = discover_outcomes(home.path());
    assert!(!outcomes.iter().any(|outcome| outcome.session().is_some()));
    assert!(outcomes.iter().any(|outcome| {
        match outcome {
            DiscoveredSession::Error {
                error: IntegrationError::Io { diagnostic, .. },
                ..
            } => diagnostic
                .verbose_chain
                .as_deref()
                .is_some_and(|chain| chain.contains("outside effective root")),
            DiscoveredSession::Error { .. } | DiscoveredSession::Session(_) => false,
        }
    }));
}

#[test]
fn discovers_modern_session_under_dated_sessions_subdir() {
    let home = codex_home();
    let workspace = home.path().join("ws");
    fs::create_dir_all(&workspace).unwrap();
    let ws_canon = workspace.canonicalize().unwrap();

    write_rollout(
        home.path(),
        "sessions/2026/08/07/rollout-abc-11111111-2222-3333-4444-555555555555.jsonl",
        &[
            session_meta(
                "11111111-2222-3333-4444-555555555555",
                ws_canon.to_str().unwrap(),
            ),
            event_msg_user("Fix the login bug"),
        ],
    );

    let sessions = discover_sessions(home.path());
    assert_eq!(sessions.len(), 1, "exactly one session discovered");
    let session = &sessions[0];
    assert_eq!(
        session.resumable_id.to_str().unwrap(),
        "11111111-2222-3333-4444-555555555555"
    );
    assert_eq!(
        session.workspace,
        WorkspaceEvidence::Recorded {
            workspace: ws_canon.clone(),
            historical_git_identity: None,
        }
    );
    assert_eq!(session.support, SupportStatus::Supported);
    assert_eq!(session.activity, ActivityStatus::Unknown);
    assert_eq!(session.risk, RiskStatus::Normal);
    assert_eq!(session.key.agent.to_str().unwrap(), "codex");
    assert_eq!(session.title.as_deref(), Some("Fix the login bug"));
}

#[test]
fn discovers_older_session_shape_with_fewer_payload_fields() {
    let home = codex_home();
    let workspace = home.path().join("old-ws");
    fs::create_dir_all(&workspace).unwrap();
    let ws_canon = workspace.canonicalize().unwrap();

    write_rollout(
        home.path(),
        "sessions/2025/01/01/rollout-old-deadbeef.jsonl",
        &[
            older_session_meta("deadbeef-old", ws_canon.to_str().unwrap()),
            event_msg_user("hello older codex"),
        ],
    );

    let sessions = discover_sessions(home.path());
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].resumable_id.to_str().unwrap(), "deadbeef-old");
    assert_eq!(
        sessions[0].workspace,
        WorkspaceEvidence::Recorded {
            workspace: ws_canon,
            historical_git_identity: None,
        }
    );
}

#[test]
fn discovers_archived_session_under_archived_sessions() {
    let home = codex_home();
    let workspace = home.path().join("arch-ws");
    fs::create_dir_all(&workspace).unwrap();
    let ws_canon = workspace.canonicalize().unwrap();

    write_rollout(
        home.path(),
        "archived_sessions/2026/01/01/rollout-arch-cafebabe.jsonl",
        &[
            session_meta("cafebabe-arch", ws_canon.to_str().unwrap()),
            event_msg_user("archived work"),
        ],
    );

    // Use discover_with_filter to also inspect the archived flag via parsed.
    let archived_parsed = {
        let found = std::cell::Cell::new(Option::<bool>::None);
        discover_with_filter(home.path(), &Bounds::default(), None, |parsed| {
            if parsed.id == "cafebabe-arch" {
                found.set(Some(parsed.archived));
            }
            true
        });
        found.into_inner()
    };
    assert_eq!(archived_parsed, Some(true));

    let sessions = discover_sessions(home.path());
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].resumable_id.to_str().unwrap(), "cafebabe-arch");
}

#[test]
fn discovers_sessions_from_both_active_and_archived_roots() {
    let home = codex_home();
    let ws1 = home.path().join("ws1");
    let ws2 = home.path().join("ws2");
    fs::create_dir_all(&ws1).unwrap();
    fs::create_dir_all(&ws2).unwrap();

    write_rollout(
        home.path(),
        "sessions/2026/08/07/rollout-a.jsonl",
        &[session_meta(
            "aaaa-active",
            ws1.canonicalize().unwrap().to_str().unwrap(),
        )],
    );
    write_rollout(
        home.path(),
        "archived_sessions/2026/08/06/rollout-b.jsonl",
        &[session_meta(
            "bbbb-archived",
            ws2.canonicalize().unwrap().to_str().unwrap(),
        )],
    );

    let sessions = discover_sessions(home.path());
    let ids: Vec<&str> = sessions
        .iter()
        .map(|s| s.resumable_id.to_str().unwrap())
        .collect();
    assert!(ids.contains(&"aaaa-active"));
    assert!(ids.contains(&"bbbb-archived"));
    assert_eq!(sessions.len(), 2);
}

#[test]
fn ignores_non_rollout_files_in_sessions_root() {
    let home = codex_home();
    let workspace = home.path().join("ws");
    fs::create_dir_all(&workspace).unwrap();

    // SQLite DB, legacy index, history, config — all ignored.
    write_rollout(
        home.path(),
        "state_5.sqlite",
        &[json!({"type": "session_meta", "payload": {"id": "sqlite-id"}})],
    );
    write_rollout(
        home.path(),
        "session_index.jsonl",
        &[json!({"type": "session_meta", "payload": {"id": "index-id"}})],
    );
    write_rollout(
        home.path(),
        "history.jsonl",
        &[json!({"type": "session_meta", "payload": {"id": "history-id"}})],
    );
    write_rollout(
        home.path(),
        "config.toml",
        &[json!({"type": "session_meta", "payload": {"id": "config-id"}})],
    );
    // Only this rollout counts.
    write_rollout(
        home.path(),
        "sessions/2026/08/07/rollout-real.jsonl",
        &[session_meta(
            "real-id",
            workspace.canonicalize().unwrap().to_str().unwrap(),
        )],
    );

    let sessions = discover_sessions(home.path());
    let ids: Vec<&str> = sessions
        .iter()
        .map(|s| s.resumable_id.to_str().unwrap())
        .collect();
    assert_eq!(ids, &["real-id"]);
}

// ---------------------------------------------------------------------------
// Identity: use payload.id, not filename or payload.session_id
// ---------------------------------------------------------------------------

#[test]
fn identity_uses_payload_id_not_filename() {
    let home = codex_home();
    let workspace = home.path().join("ws");
    fs::create_dir_all(&workspace).unwrap();

    // Filename suggests one UUID, payload.id has another.
    write_rollout(
        home.path(),
        "sessions/2026/08/07/rollout-2026-WRONGUUID.jsonl",
        &[json!({
            "type": "session_meta",
            "payload": {
                "id": "REAL-UUID-PAYLOAD",
                "cwd": workspace.canonicalize().unwrap().to_str().unwrap()
            }
        })],
    );

    let sessions = discover_sessions(home.path());
    assert_eq!(sessions.len(), 1);
    assert_eq!(
        sessions[0].resumable_id.to_str().unwrap(),
        "REAL-UUID-PAYLOAD"
    );
}

#[test]
fn identity_ignores_payload_session_id_field() {
    let home = codex_home();
    let workspace = home.path().join("ws");
    fs::create_dir_all(&workspace).unwrap();

    // payload.session_id is an unrelated field; payload.id is authoritative.
    write_rollout(
        home.path(),
        "sessions/2026/08/07/rollout-x.jsonl",
        &[json!({
            "type": "session_meta",
            "payload": {
                "id": "AUTHORITATIVE-ID",
                "session_id": "DISTRACTOR-ID",
                "cwd": workspace.canonicalize().unwrap().to_str().unwrap()
            }
        })],
    );

    let sessions = discover_sessions(home.path());
    assert_eq!(sessions.len(), 1);
    assert_eq!(
        sessions[0].resumable_id.to_str().unwrap(),
        "AUTHORITATIVE-ID"
    );
}

#[test]
fn key_includes_effective_root_and_canonical_rollout_path() {
    let home = codex_home();
    let workspace = home.path().join("ws");
    fs::create_dir_all(&workspace).unwrap();

    let rel = "sessions/2026/08/07/rollout-key.jsonl";
    write_rollout(
        home.path(),
        rel,
        &[session_meta(
            "key-id",
            workspace.canonicalize().unwrap().to_str().unwrap(),
        )],
    );

    let sessions = discover_sessions(home.path());
    assert_eq!(sessions.len(), 1);
    let key = &sessions[0].key;
    // effective_root is the canonical CODEX_HOME.
    assert_eq!(key.effective_root, home.path().canonicalize().unwrap());
    // native_locator embeds the id and canonical rollout path.
    let locator = key.native_locator.to_str().unwrap();
    assert!(locator.starts_with("key-id::"));
    assert!(locator.contains("rollout-key.jsonl"));
}

// ---------------------------------------------------------------------------
// Alternate CODEX_HOME
// ---------------------------------------------------------------------------

#[test]
fn alternate_codex_home_is_used_when_env_set() {
    // Set CODEX_HOME to a temp dir and confirm discovery targets it.
    let alt = tempfile::tempdir().unwrap();
    let workspace = alt.path().join("ws");
    fs::create_dir_all(&workspace).unwrap();

    write_rollout(
        alt.path(),
        "sessions/2026/08/07/rollout-alt.jsonl",
        &[session_meta(
            "alt-id",
            workspace.canonicalize().unwrap().to_str().unwrap(),
        )],
    );

    let saved = std::env::var_os(ENV_CODEX_HOME);
    // SAFETY: this test is not run in parallel with other CODEX_HOME-dependent
    // tests; we restore the prior value before asserting. Edition 2024 marks
    // env mutation as unsafe because it can affect other threads.
    unsafe {
        std::env::set_var(ENV_CODEX_HOME, alt.path());
    }

    let root = effective_root().expect("effective root resolves");
    let sessions = discover(&root, &Bounds::default())
        .into_iter()
        .filter_map(|o| o.session().cloned())
        .collect::<Vec<_>>();

    // Restore env.
    // SAFETY: restoring the previously captured value.
    unsafe {
        match saved {
            Some(v) => std::env::set_var(ENV_CODEX_HOME, v),
            None => std::env::remove_var(ENV_CODEX_HOME),
        }
    }

    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].resumable_id.to_str().unwrap(), "alt-id");
    assert_eq!(
        sessions[0].key.effective_root,
        alt.path().canonicalize().unwrap()
    );
}

#[test]
fn resume_preserves_alternate_codex_home_as_env_override() {
    let alt = tempfile::tempdir().unwrap();
    let workspace = alt.path().join("ws");
    fs::create_dir_all(&workspace).unwrap();
    let ws_canon = workspace.canonicalize().unwrap();

    let saved = std::env::var_os(ENV_CODEX_HOME);
    // SAFETY: see test above; restored before assertions.
    unsafe {
        std::env::set_var(ENV_CODEX_HOME, alt.path());
    }

    write_rollout(
        alt.path(),
        "sessions/2026/08/07/rollout-resume.jsonl",
        &[session_meta("resume-id", ws_canon.to_str().unwrap())],
    );

    let root = effective_root().unwrap();
    let session = discover(&root, &Bounds::default())
        .into_iter()
        .filter_map(|o| o.session().cloned())
        .next()
        .unwrap();

    let spec = resume_spec(&session, &dirs_home().unwrap().join(".codex"));
    assert_eq!(spec.program.to_str().unwrap(), "codex");
    assert_eq!(
        spec.argv
            .iter()
            .map(|a| a.to_str().unwrap().to_string())
            .collect::<Vec<_>>(),
        vec!["-C", ws_canon.to_str().unwrap(), "resume", "resume-id"]
    );
    assert_eq!(spec.cwd, ws_canon);
    // Nondefault root is preserved as CODEX_HOME override (canonicalized by
    // discovery for identity stability).
    let alt_canon = alt.path().canonicalize().unwrap();
    let has_home_override = spec
        .env
        .iter()
        .any(|(k, v)| k == ENV_CODEX_HOME && v == alt_canon.as_os_str());
    assert!(has_home_override, "alternate CODEX_HOME must be preserved");

    // SAFETY: restoring the previously captured value.
    unsafe {
        match saved {
            Some(v) => std::env::set_var(ENV_CODEX_HOME, v),
            None => std::env::remove_var(ENV_CODEX_HOME),
        }
    }
}

#[test]
fn resume_omits_env_override_for_default_codex_home() {
    let home = codex_home();
    let workspace = home.path().join("ws");
    fs::create_dir_all(&workspace).unwrap();
    let ws_canon = workspace.canonicalize().unwrap();

    write_rollout(
        home.path(),
        "sessions/2026/08/07/rollout-def.jsonl",
        &[session_meta("def-id", ws_canon.to_str().unwrap())],
    );

    let session = discover_sessions(home.path()).pop().unwrap();
    // No CODEX_HOME set: effective_root falls back to ~/.codex, which differs
    // from the temp home, so an override is expected here. Use a default_home
    // equal to the temp home to assert the override is omitted.
    let spec = resume_spec(&session, &home.path().canonicalize().unwrap());
    assert!(spec.env.is_empty(), "no override when root is default");
}

// ---------------------------------------------------------------------------
// Duplicate user representation
// ---------------------------------------------------------------------------

#[test]
fn deduplicates_paired_event_msg_and_response_item_user_messages() {
    let home = codex_home();
    let workspace = home.path().join("ws");
    fs::create_dir_all(&workspace).unwrap();

    // Same user input represented as BOTH event_msg and response_item.
    write_rollout(
        home.path(),
        "sessions/2026/08/07/rollout-dup.jsonl",
        &[
            session_meta(
                "dup-id",
                workspace.canonicalize().unwrap().to_str().unwrap(),
            ),
            event_msg_user("Please refactor this"),
            response_item_user("Please refactor this"),
        ],
    );

    let session = discover_sessions(home.path()).pop().unwrap();
    // Title derived from the single deduped message.
    assert_eq!(session.title.as_deref(), Some("Please refactor this"));
}

#[test]
fn keeps_distinct_user_messages_in_order() {
    let home = codex_home();
    let workspace = home.path().join("ws");
    fs::create_dir_all(&workspace).unwrap();

    write_rollout(
        home.path(),
        "sessions/2026/08/07/rollout-multi.jsonl",
        &[
            session_meta(
                "multi-id",
                workspace.canonicalize().unwrap().to_str().unwrap(),
            ),
            event_msg_user("first question"),
            event_msg_user("second question"),
            response_item_user("first question"), // dup, dropped
            event_msg_user("third question"),
        ],
    );

    let session = discover_sessions(home.path()).pop().unwrap();
    assert_eq!(session.title.as_deref(), Some("first question"));
}

#[test]
fn excludes_assistant_and_developer_messages() {
    let home = codex_home();
    let workspace = home.path().join("ws");
    fs::create_dir_all(&workspace).unwrap();

    write_rollout(
        home.path(),
        "sessions/2026/08/07/rollout-inject.jsonl",
        &[
            session_meta(
                "inject-id",
                workspace.canonicalize().unwrap().to_str().unwrap(),
            ),
            response_item_developer("You are a helpful agent"), // excluded
            event_msg_user("real user prompt"),
            response_item_assistant("Sure, here is the plan"), // excluded
        ],
    );

    let session = discover_sessions(home.path()).pop().unwrap();
    assert_eq!(session.title.as_deref(), Some("real user prompt"));
}

// ---------------------------------------------------------------------------
// Attachments
// ---------------------------------------------------------------------------

#[test]
fn supports_image_attachment_placeholder_without_base64() {
    let home = codex_home();
    let workspace = home.path().join("ws");
    fs::create_dir_all(&workspace).unwrap();

    write_rollout(
        home.path(),
        "sessions/2026/08/07/rollout-img.jsonl",
        &[
            session_meta(
                "img-id",
                workspace.canonicalize().unwrap().to_str().unwrap(),
            ),
            json!({
                "type": "event_msg",
                "payload": {
                    "type": "user_message",
                    "message": {
                        "role": "user",
                        "content": [
                            { "type": "input_text", "text": "look at this" },
                            { "type": "input_image", "media_type": "image/png", "data": "iVBORw0KGgoAAAANSUh" }
                        ]
                    }
                }
            }),
        ],
    );

    let session = discover_sessions(home.path()).pop().unwrap();
    // Title is the text portion; base64 never leaks into the title.
    assert_eq!(session.title.as_deref(), Some("look at this"));
    assert!(!session.title.as_deref().unwrap_or("").contains("iVBOR"));
}

// ---------------------------------------------------------------------------
// Unknown records and robustness
// ---------------------------------------------------------------------------

#[test]
fn tolerates_unknown_record_types() {
    let home = codex_home();
    let workspace = home.path().join("ws");
    fs::create_dir_all(&workspace).unwrap();

    write_rollout(
        home.path(),
        "sessions/2026/08/07/rollout-unknown.jsonl",
        &[
            json!({ "type": "unknown_future_type", "payload": { "foo": "bar" } }),
            session_meta(
                "unk-id",
                workspace.canonicalize().unwrap().to_str().unwrap(),
            ),
            event_msg_user("survives unknown record"),
        ],
    );

    let session = discover_sessions(home.path()).pop().unwrap();
    assert_eq!(session.title.as_deref(), Some("survives unknown record"));
}

#[test]
fn noninteractive_file_without_session_meta_is_not_discovered() {
    let home = codex_home();
    // A file that only contains response items and no session_meta header.
    write_rollout(
        home.path(),
        "sessions/2026/08/07/rollout-noninteractive.jsonl",
        &[
            json!({ "type": "response_item", "payload": { "type": "message", "message": { "role": "user", "content": "hi" } } }),
        ],
    );

    let sessions = discover_sessions(home.path());
    assert!(sessions.is_empty(), "no session_meta => not discovered");
    // And it is not reported as an error either.
    assert!(discover_outcomes(home.path()).is_empty());
}

// ---------------------------------------------------------------------------
// Missing Workspace
// ---------------------------------------------------------------------------

#[test]
fn missing_workspace_yields_unknown_workspace_evidence() {
    let home = codex_home();

    write_rollout(
        home.path(),
        "sessions/2026/08/07/rollout-nocwd.jsonl",
        &[json!({
            "type": "session_meta",
            "payload": { "id": "nocwd-id" }
        })],
    );

    let session = discover_sessions(home.path()).pop().unwrap();
    assert_eq!(session.workspace, WorkspaceEvidence::Unknown);
    assert_eq!(session.resumable_id.to_str().unwrap(), "nocwd-id");
}

#[test]
fn workspace_from_workspace_roots_is_not_used_for_resume_cwd() {
    let home = codex_home();
    let actual_ws = home.path().join("actual");
    let other_root = home.path().join("other-root");
    fs::create_dir_all(&actual_ws).unwrap();
    fs::create_dir_all(&other_root).unwrap();

    // payload.cwd is authoritative; workspace_roots is additional only.
    write_rollout(
        home.path(),
        "sessions/2026/08/07/rollout-roots.jsonl",
        &[json!({
            "type": "session_meta",
            "payload": {
                "id": "roots-id",
                "cwd": actual_ws.canonicalize().unwrap().to_str().unwrap(),
                "workspace_roots": [other_root.canonicalize().unwrap().to_str().unwrap()]
            }
        })],
    );

    let session = discover_sessions(home.path()).pop().unwrap();
    match &session.workspace {
        WorkspaceEvidence::Recorded { workspace, .. } => {
            assert_eq!(workspace, &actual_ws.canonicalize().unwrap());
        }
        _ => panic!("expected recorded workspace"),
    }
}

// ---------------------------------------------------------------------------
// Malformed and truncated files
// ---------------------------------------------------------------------------

#[test]
fn malformed_middle_record_is_isolated_not_aborted() {
    let home = codex_home();
    let workspace = home.path().join("ws");
    fs::create_dir_all(&workspace).unwrap();

    let path = home.path().join("sessions/2026/08/07/rollout-mid.jsonl");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let content = format!(
        "{}\n{{this is not valid json}}\n{}\n",
        serde_json::to_string(&session_meta(
            "mid-id",
            workspace.canonicalize().unwrap().to_str().unwrap()
        ))
        .unwrap(),
        serde_json::to_string(&event_msg_user("after malformed")).unwrap()
    );
    fs::write(&path, content.as_bytes()).unwrap();

    let session = discover_sessions(home.path()).pop().unwrap();
    assert_eq!(session.resumable_id.to_str().unwrap(), "mid-id");
    assert_eq!(session.title.as_deref(), Some("after malformed"));
}

#[test]
fn truncated_tail_is_treated_as_incomplete_not_error() {
    let home = codex_home();
    let workspace = home.path().join("ws");
    fs::create_dir_all(&workspace).unwrap();

    let path = home.path().join("sessions/2026/08/07/rollout-trunc.jsonl");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut content = serde_json::to_string(&session_meta(
        "trunc-id",
        workspace.canonicalize().unwrap().to_str().unwrap(),
    ))
    .unwrap();
    content.push('\n');
    content.push_str("{\"type\":\"event_msg\",\"payload\":{\"type\":\"user_message\",\"me"); // no newline
    fs::write(&path, content.as_bytes()).unwrap();

    let outcomes = discover_outcomes(home.path());
    assert_eq!(outcomes.len(), 1);
    assert!(
        outcomes[0].session().is_some(),
        "truncated tail is not an error"
    );
    let session = outcomes[0].session().unwrap();
    assert_eq!(session.resumable_id.to_str().unwrap(), "trunc-id");
}

#[test]
fn malformed_header_yields_error_outcome_isolated_from_other_files() {
    let home = codex_home();
    let workspace = home.path().join("ws");
    fs::create_dir_all(&workspace).unwrap();

    // File 1: session_meta with a non-object payload => InvalidSession.
    write_rollout(
        home.path(),
        "sessions/2026/08/07/rollout-bad.jsonl",
        &[json!({ "type": "session_meta", "payload": "not an object" })],
    );
    // File 2: valid, still discovered despite File 1 failing.
    write_rollout(
        home.path(),
        "sessions/2026/08/07/rollout-good.jsonl",
        &[session_meta(
            "good-id",
            workspace.canonicalize().unwrap().to_str().unwrap(),
        )],
    );

    let outcomes = discover_outcomes(home.path());
    let errors = outcomes
        .iter()
        .filter(|o| matches!(o, DiscoveredSession::Error { .. }))
        .count();
    let good = outcomes
        .iter()
        .filter_map(|o| o.session())
        .filter(|s| s.resumable_id.to_str().unwrap() == "good-id")
        .count();
    assert_eq!(errors, 1, "the malformed file is isolated as an error");
    assert_eq!(good, 1, "the valid file is still discovered");
}

#[test]
fn missing_payload_id_yields_error_outcome() {
    let home = codex_home();

    write_rollout(
        home.path(),
        "sessions/2026/08/07/rollout-noid.jsonl",
        &[json!({ "type": "session_meta", "payload": { "cwd": "/tmp" } })],
    );

    let outcomes = discover_outcomes(home.path());
    assert!(matches!(
        outcomes.first(),
        Some(DiscoveredSession::Error { .. })
    ));
}

// ---------------------------------------------------------------------------
// Discovery works with state DB / indexes absent, stale, or corrupt
// ---------------------------------------------------------------------------

#[test]
fn discovery_ignores_corrupt_sqlite_and_legacy_indexes() {
    let home = codex_home();
    let workspace = home.path().join("ws");
    fs::create_dir_all(&workspace).unwrap();

    // Corrupt SQLite-like file, corrupt index, corrupt history.
    fs::write(
        home.path().join("state_5.sqlite"),
        b"not a real sqlite db garbage",
    )
    .unwrap();
    fs::write(home.path().join("session_index.jsonl"), b"{ broken index }").unwrap();
    fs::write(home.path().join("history.jsonl"), b"{ broken history }").unwrap();

    write_rollout(
        home.path(),
        "sessions/2026/08/07/rollout-robust.jsonl",
        &[session_meta(
            "robust-id",
            workspace.canonicalize().unwrap().to_str().unwrap(),
        )],
    );

    let sessions = discover_sessions(home.path());
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].resumable_id.to_str().unwrap(), "robust-id");
}

#[test]
fn discovery_does_not_read_or_modify_sqlite_index_or_history() {
    let home = codex_home();
    let workspace = home.path().join("ws");
    fs::create_dir_all(&workspace).unwrap();

    let sqlite_bytes = b"SQLITE-GUARD-BYTES-1234";
    let index_bytes = b"INDEX-GUARD-BYTES-5678";
    let history_bytes = b"HISTORY-GUARD-BYTES-9012";

    fs::write(home.path().join("state_5.sqlite"), sqlite_bytes).unwrap();
    fs::write(home.path().join("session_index.jsonl"), index_bytes).unwrap();
    fs::write(home.path().join("history.jsonl"), history_bytes).unwrap();

    // Snapshot before.
    let before_sqlite = snapshot::snapshot_file(&home.path().join("state_5.sqlite")).unwrap();
    let before_index = snapshot::snapshot_file(&home.path().join("session_index.jsonl")).unwrap();
    let before_history = snapshot::snapshot_file(&home.path().join("history.jsonl")).unwrap();

    write_rollout(
        home.path(),
        "sessions/2026/08/07/rollout-guard.jsonl",
        &[session_meta(
            "guard-id",
            workspace.canonicalize().unwrap().to_str().unwrap(),
        )],
    );
    let rollout_path = home.path().join("sessions/2026/08/07/rollout-guard.jsonl");
    let before_rollout = snapshot::snapshot_file(&rollout_path).unwrap();

    // Discover.
    let sessions = discover_sessions(home.path());
    assert_eq!(sessions.len(), 1);

    // Snapshot after: nothing changed.
    snapshot::assert_file_unchanged(
        &before_sqlite,
        &snapshot::snapshot_file(&home.path().join("state_5.sqlite")).unwrap(),
    );
    snapshot::assert_file_unchanged(
        &before_index,
        &snapshot::snapshot_file(&home.path().join("session_index.jsonl")).unwrap(),
    );
    snapshot::assert_file_unchanged(
        &before_history,
        &snapshot::snapshot_file(&home.path().join("history.jsonl")).unwrap(),
    );
    snapshot::assert_file_unchanged(
        &before_rollout,
        &snapshot::snapshot_file(&rollout_path).unwrap(),
    );
}

#[test]
fn discovery_is_read_only_on_rollout_files() {
    let home = codex_home();
    let workspace = home.path().join("ws");
    fs::create_dir_all(&workspace).unwrap();

    write_rollout(
        home.path(),
        "sessions/2026/08/07/rollout-ro.jsonl",
        &[
            session_meta("ro-id", workspace.canonicalize().unwrap().to_str().unwrap()),
            event_msg_user("preserve my bytes"),
        ],
    );
    let rollout_path = home.path().join("sessions/2026/08/07/rollout-ro.jsonl");
    let before = snapshot::snapshot_file(&rollout_path).unwrap();

    let _sessions = discover_sessions(home.path());

    let after = snapshot::snapshot_file(&rollout_path).unwrap();
    snapshot::assert_file_unchanged(&before, &after);
}

// ---------------------------------------------------------------------------
// Resume command (fake exact argv/env)
// ---------------------------------------------------------------------------

#[test]
fn resume_command_is_codex_dash_c_workspace_resume_uuid() {
    let home = codex_home();
    let workspace = home.path().join("ws");
    fs::create_dir_all(&workspace).unwrap();
    let ws_canon = workspace.canonicalize().unwrap();

    write_rollout(
        home.path(),
        "sessions/2026/08/07/rollout-cmd.jsonl",
        &[session_meta(
            "11111111-2222-3333-4444-555555555555",
            ws_canon.to_str().unwrap(),
        )],
    );

    let session = discover_sessions(home.path()).pop().unwrap();
    let spec = resume_spec(&session, &home.path().canonicalize().unwrap());

    assert_eq!(spec.program.to_str().unwrap(), "codex");
    let argv: Vec<String> = spec
        .argv
        .iter()
        .map(|a| a.to_str().unwrap().to_string())
        .collect();
    assert_eq!(
        argv,
        vec![
            String::from("-C"),
            ws_canon.to_str().unwrap().to_string(),
            String::from("resume"),
            String::from("11111111-2222-3333-4444-555555555555"),
        ]
    );
    assert_eq!(spec.cwd, ws_canon);
}

#[test]
fn resume_does_not_use_rollout_path_as_locator() {
    let home = codex_home();
    let workspace = home.path().join("ws");
    fs::create_dir_all(&workspace).unwrap();

    write_rollout(
        home.path(),
        "sessions/2026/08/07/rollout-loc.jsonl",
        &[session_meta(
            "loc-id",
            workspace.canonicalize().unwrap().to_str().unwrap(),
        )],
    );

    let session = discover_sessions(home.path()).pop().unwrap();
    let spec = resume_spec(&session, &home.path().canonicalize().unwrap());
    let argv_joined = spec
        .argv
        .iter()
        .map(|a| a.to_str().unwrap().to_string())
        .collect::<Vec<_>>()
        .join(" ");
    // The rollout file path must not appear as the resume locator.
    assert!(!argv_joined.contains(".jsonl"));
    assert!(argv_joined.contains("resume loc-id"));
}

// ---------------------------------------------------------------------------
// Import / thread metadata as safe badges (no path/remote display)
// ---------------------------------------------------------------------------

#[test]
fn imported_session_uses_new_rollout_id_for_resume() {
    let home = codex_home();
    let workspace = home.path().join("ws");
    fs::create_dir_all(&workspace).unwrap();

    write_rollout(
        home.path(),
        "sessions/2026/08/07/rollout-import.jsonl",
        &[
            json!({
                "type": "session_meta",
                "payload": {
                    "id": "NEW-OMP-OR-CODEX-ID",
                    "cwd": workspace.canonicalize().unwrap().to_str().unwrap(),
                    "source": "imported",
                    "thread_source": "claude"
                }
            }),
            json!({
                "type": "session_meta",
                "payload": {
                    "foreign_session_import": {
                        "source_kind": "claude",
                        "origin_path": "/home/user/.claude/projects/secret/abc.jsonl",
                        "origin_remote": "git@github.com:user/private.git"
                    }
                }
            }),
            event_msg_user("imported conversation"),
        ],
    );

    let session = discover_sessions(home.path()).pop().unwrap();
    // Resume uses the NEW rollout id, never the origin.
    assert_eq!(
        session.resumable_id.to_str().unwrap(),
        "NEW-OMP-OR-CODEX-ID"
    );

    let spec = resume_spec(&session, &home.path().canonicalize().unwrap());
    let argv = spec
        .argv
        .iter()
        .map(|a| a.to_str().unwrap().to_string())
        .collect::<Vec<_>>()
        .join(" ");
    assert!(argv.contains("NEW-OMP-OR-CODEX-ID"));
    // The origin path/remote never reach the launch command.
    assert!(!argv.contains("secret"));
    assert!(!argv.contains("private.git"));
    // And never reach the title.
    assert!(
        !session
            .title
            .as_deref()
            .unwrap_or("")
            .contains("private.git")
    );
    // The safe coarse badge IS surfaced, though.
    assert!(
        session
            .title
            .as_deref()
            .unwrap_or("")
            .contains("imported from claude"),
        "title missing import badge: {:?}",
        session.title
    );
}

#[test]
fn thread_metadata_does_not_display_remote_or_path() {
    let home = codex_home();
    let workspace = home.path().join("ws");
    fs::create_dir_all(&workspace).unwrap();

    write_rollout(
        home.path(),
        "sessions/2026/08/07/rollout-thread.jsonl",
        &[
            json!({
                "type": "session_meta",
                "payload": {
                    "id": "thread-id",
                    "cwd": workspace.canonicalize().unwrap().to_str().unwrap(),
                    "parent_thread_id": "parent-uuid",
                    "git": { "remote": "git@github.com:user/secret.git", "branch": "main" }
                }
            }),
            event_msg_user("threaded work"),
        ],
    );

    let session = discover_sessions(home.path()).pop().unwrap();
    let title = session.title.as_deref().unwrap_or("");
    assert!(!title.contains("secret.git"));
    assert!(!title.contains("git@github.com"));
}

// ---------------------------------------------------------------------------
// Filter (Scope membership) integration
// ---------------------------------------------------------------------------

#[test]
fn discover_with_filter_excludes_out_of_scope_workspaces() {
    let home = codex_home();
    let ws_in = home.path().join("in-scope");
    let ws_out = home.path().join("out-scope");
    fs::create_dir_all(&ws_in).unwrap();
    fs::create_dir_all(&ws_out).unwrap();

    write_rollout(
        home.path(),
        "sessions/2026/08/07/rollout-in.jsonl",
        &[session_meta(
            "in-id",
            ws_in.canonicalize().unwrap().to_str().unwrap(),
        )],
    );
    write_rollout(
        home.path(),
        "sessions/2026/08/07/rollout-out.jsonl",
        &[session_meta(
            "out-id",
            ws_out.canonicalize().unwrap().to_str().unwrap(),
        )],
    );

    let allowed = ws_in.canonicalize().unwrap();
    let sessions: Vec<Session> =
        discover_with_filter(home.path(), &Bounds::default(), None, |parsed| {
            parsed.cwd.as_ref() == Some(&allowed)
        })
        .into_iter()
        .filter_map(|o| o.session().cloned())
        .collect();

    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].resumable_id.to_str().unwrap(), "in-id");
}
#[test]
fn workspace_gate_skips_out_of_scope_rollouts_and_never_alters_kept_output() {
    let home = codex_home();
    let ws_in = home.path().join("in-scope");
    let ws_out = home.path().join("out-scope");
    fs::create_dir_all(&ws_in).unwrap();
    fs::create_dir_all(&ws_out).unwrap();
    let in_cwd = ws_in.canonicalize().unwrap();
    let out_cwd = ws_out.canonicalize().unwrap();

    // In-scope rollout whose first user message sits well past the 4 KiB
    // gate-read budget: proves the gate read is never used as a parse
    // source (the title must still come from the 64 KiB ladder).
    let filler = "x".repeat(200);
    let mut records = vec![session_meta("in-id", in_cwd.to_str().unwrap())];
    for _ in 0..40 {
        records.push(json!({"type": "event_msg", "payload": {"type": "other", "filler": filler}}));
    }
    records.push(event_msg_user("late title"));
    write_rollout(
        home.path(),
        "sessions/2026/08/07/rollout-in.jsonl",
        &records,
    );
    write_rollout(
        home.path(),
        "sessions/2026/08/07/rollout-out.jsonl",
        &[
            session_meta("out-id", out_cwd.to_str().unwrap()),
            event_msg_user("never needed"),
        ],
    );
    // A rollout with no cwd is never offered to the gate; the post-parse
    // filter decides its fate.
    write_rollout(
        home.path(),
        "sessions/2026/08/07/rollout-nocwd.jsonl",
        &[json!({"type": "session_meta", "payload": {"id": "nocwd-id"}})],
    );

    let gate = |cwd: &std::path::Path| cwd == in_cwd;
    let outcomes = discover_with_filter(home.path(), &Bounds::default(), Some(&gate), |_| true);
    let mut ids: Vec<String> = outcomes
        .iter()
        .filter_map(|o| {
            o.session()
                .map(|s| s.resumable_id.to_string_lossy().into_owned())
        })
        .collect();
    ids.sort();
    assert_eq!(
        ids,
        ["in-id", "nocwd-id"],
        "gate skips only out-of-scope cwd"
    );
    assert!(
        !outcomes
            .iter()
            .any(|o| matches!(o, DiscoveredSession::Error { .. })),
        "gate rejection is silent, never an error"
    );

    // Byte-identical kept output vs the ungated path.
    let kept = outcomes
        .iter()
        .filter_map(|o| o.session())
        .find(|s| s.resumable_id.to_str() == Some("in-id"))
        .cloned()
        .unwrap();
    assert_eq!(kept.title.as_deref(), Some("late title"));
    let ungated = discover_with_filter(home.path(), &Bounds::default(), None, |parsed| {
        parsed.id == "in-id"
    })
    .into_iter()
    .filter_map(|o| o.session().cloned())
    .next()
    .unwrap();
    assert_eq!(kept, ungated, "gated output must match ungated output");
}
#[test]
fn workspace_gate_never_rejects_relative_or_unresolvable_cwd() {
    let home = codex_home();
    // A relative cwd fails `canonicalize_workspace` and becomes
    // `parsed.cwd == None` in the full parse, which the post-parse filter
    // keeps unconditionally -- the gate must not silently drop it.
    write_rollout(
        home.path(),
        "sessions/2026/08/07/rollout-rel.jsonl",
        &[session_meta("rel-id", "relative/workspace")],
    );

    let reject_all = |_: &std::path::Path| false;
    let ids: Vec<String> =
        discover_with_filter(home.path(), &Bounds::default(), Some(&reject_all), |_| true)
            .into_iter()
            .filter_map(|o| {
                o.session()
                    .map(|s| s.resumable_id.to_string_lossy().into_owned())
            })
            .collect();
    assert_eq!(
        ids,
        ["rel-id"],
        "relative cwd must survive a reject-all gate"
    );
}

// ---------------------------------------------------------------------------
// Filename recognition helper
// ---------------------------------------------------------------------------

#[test]
fn is_rollout_filename_recognizes_rollout_jsonl() {
    assert!(is_rollout_filename(Some(std::ffi::OsStr::new(
        "rollout-abc-123.jsonl"
    ))));
    assert!(!is_rollout_filename(Some(std::ffi::OsStr::new(
        "history.jsonl"
    ))));
    assert!(!is_rollout_filename(Some(std::ffi::OsStr::new(
        "rollout.txt"
    ))));
    assert!(!is_rollout_filename(Some(std::ffi::OsStr::new(
        "state_5.sqlite"
    ))));
    assert!(!is_rollout_filename(None));
}

// ---------------------------------------------------------------------------
// Effective root resolution
// ---------------------------------------------------------------------------

#[test]
fn effective_root_defaults_to_home_codex_when_env_unset() {
    let saved = std::env::var_os(ENV_CODEX_HOME);
    let saved_home = std::env::var_os("HOME");

    // SAFETY: isolated test; values restored below.
    unsafe {
        std::env::remove_var(ENV_CODEX_HOME);
        std::env::set_var("HOME", "/tmp/fake-home");
    }
    let root = effective_root().unwrap();
    assert_eq!(root, PathBuf::from("/tmp/fake-home/.codex"));

    // SAFETY: restoring previously captured values.
    unsafe {
        match saved {
            Some(v) => std::env::set_var(ENV_CODEX_HOME, v),
            None => std::env::remove_var(ENV_CODEX_HOME),
        }
        match saved_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
    }
}

#[test]
fn rollout_roots_returns_only_existing_dirs() {
    let home = codex_home();
    fs::create_dir_all(home.path().join("sessions")).unwrap();
    // archived_sessions does not exist.

    let roots = rollout_roots(home.path());
    assert_eq!(roots.len(), 1);
    assert_eq!(roots[0].kind, RolloutKind::Active);
}

// ---------------------------------------------------------------------------
// Discovery-time bounded early read (large rollouts)
// ---------------------------------------------------------------------------

/// A large outlier rollout (tens of MB, common after long-lived real usage)
/// previously had its *entire* content read and every line JSON-parsed
/// during discovery purely to find `session_meta` and derive a title from
/// the first user message. `parse_rollout_file`'s bounded early read must
/// still discover the Session and its correct title, from a fast path that
/// reads only the first ~1 MiB, not the whole file.
#[test]
fn large_rollout_title_is_derived_from_bounded_early_read() {
    let home = codex_home();
    let workspace = home.path().join("ws");
    fs::create_dir_all(&workspace).unwrap();
    let ws_canon = workspace.canonicalize().unwrap();

    let mut records = vec![session_meta("large-early-id", ws_canon.to_str().unwrap())];
    records.push(event_msg_user("Fix the login bug"));
    // Pad well past the 1 MiB early-read budget with filler assistant
    // records the early read must never need to reach. One 4 KiB record
    // serializes to a known, fixed length, so the target count is computed
    // once up front instead of re-serializing the growing Vec every
    // iteration (which was O(n^2) and made this test take ~10s).
    let filler = "x".repeat(4096);
    let filler_record = json!({
        "timestamp": "2026-08-07T10:00:02.000Z",
        "type": "response_item",
        "payload": {
            "type": "message",
            "role": "assistant",
            "content": [{ "type": "output_text", "text": filler }]
        }
    });
    let filler_len = filler_record.to_string().len() + 1;
    let filler_count = (2usize * 1024 * 1024).div_ceil(filler_len);
    records.extend(std::iter::repeat_n(filler_record, filler_count));

    write_rollout(
        home.path(),
        "sessions/2026/08/07/rollout-large.jsonl",
        &records,
    );

    let sessions = discover_sessions(home.path());
    assert_eq!(sessions.len(), 1, "exactly one session discovered");
    assert_eq!(sessions[0].resumable_id.to_str().unwrap(), "large-early-id");
    assert_eq!(sessions[0].title.as_deref(), Some("Fix the login bug"));
}

/// Anomalous shape: `session_meta` appears *after* the 1 MiB early-read
/// budget (unlike every real Codex rollout, where it is the first record).
/// `parse_rollout_file` must still discover the Session correctly via its
/// full-read fallback path, proving the bounded fast path never silently
/// drops a Session it cannot fully parse from the first ~1 MiB alone.
#[test]
fn session_meta_beyond_early_read_budget_falls_back_to_full_read() {
    let home = codex_home();
    let workspace = home.path().join("ws");
    fs::create_dir_all(&workspace).unwrap();
    let ws_canon = workspace.canonicalize().unwrap();

    // One 4 KiB record serializes to a known, fixed length, so the target
    // count is computed once up front instead of re-serializing the
    // growing Vec every iteration (which was O(n^2) and made this test
    // take several seconds).
    let filler = "y".repeat(4096);
    let filler_record = json!({
        "timestamp": "2026-08-07T10:00:00.000Z",
        "type": "turn_context",
        "payload": { "note": filler }
    });
    let filler_len = filler_record.to_string().len() + 1;
    let filler_count = (2usize * 1024 * 1024).div_ceil(filler_len);
    let mut records: Vec<serde_json::Value> =
        std::iter::repeat_n(filler_record, filler_count).collect();
    records.push(session_meta("late-header-id", ws_canon.to_str().unwrap()));
    records.push(event_msg_user("late header still discovered"));

    write_rollout(
        home.path(),
        "sessions/2026/08/07/rollout-late-header.jsonl",
        &records,
    );

    let sessions = discover_sessions(home.path());
    assert_eq!(sessions.len(), 1, "exactly one session discovered");
    assert_eq!(sessions[0].resumable_id.to_str().unwrap(), "late-header-id");
    assert_eq!(
        sessions[0].title.as_deref(),
        Some("late header still discovered")
    );
}

// ---------------------------------------------------------------------------
// Discovery cache correctness
// ---------------------------------------------------------------------------

/// A fresh cache and a warm (second-run) cache must discover byte-identical
/// Sessions from the same corpus: the cache is purely a discovery-speed
/// optimization, never a source of truth. This runs `discover_with_filter_
/// enriched` twice -- once against a `DiscoveryCache` backed by a real file
/// that starts empty (populating and saving it), once against a fresh
/// `DiscoveryCache::load` of that same now-populated file (an all-hits
/// warm run) -- and asserts the two full `Session` lists are equal.
#[test]
fn cached_and_uncached_discovery_produce_identical_sessions() {
    let home = codex_home();
    let workspace = home.path().join("ws");
    fs::create_dir_all(&workspace).unwrap();
    let ws_canon = workspace.canonicalize().unwrap();

    write_rollout(
        home.path(),
        "sessions/2026/08/07/rollout-one.jsonl",
        &[
            session_meta("cache-one", ws_canon.to_str().unwrap()),
            event_msg_user("first session message"),
        ],
    );
    write_rollout(
        home.path(),
        "archived_sessions/2026/01/01/rollout-two.jsonl",
        &[
            session_meta("cache-two", ws_canon.to_str().unwrap()),
            event_msg_user("second session message"),
        ],
    );
    // A rollout with no session_meta at all -- must be cached as a
    // definitive "no session" and still absent from both runs.
    write_rollout(
        home.path(),
        "sessions/2026/08/07/rollout-empty.jsonl",
        &[json!({ "type": "turn_context", "payload": {} })],
    );

    let cache_dir = tempfile::tempdir().unwrap();
    let cache_path = cache_dir.path().join("codex-discovery-v1.json");

    let discover_all = |cache: &cache::DiscoveryCache| -> Vec<Session> {
        let (outcomes, _) = discover_with_filter_enriched(
            home.path(),
            &Bounds::default(),
            None,
            |_| true,
            Some(cache),
        );
        outcomes
            .into_iter()
            .filter_map(|o| o.session().cloned())
            .collect()
    };

    let cold_cache = cache::DiscoveryCache::load(Some(cache_path.clone()));
    let cold = discover_all(&cold_cache);
    cold_cache.save();
    assert_eq!(cold.len(), 2, "exactly the two real sessions, not the empty rollout");

    // A second, independently loaded cache over the same now-populated
    // file: every rollout must be an all-hits cache lookup, never a re-read.
    let warm_cache = cache::DiscoveryCache::load(Some(cache_path));
    let warm = discover_all(&warm_cache);

    assert_eq!(cold, warm, "cached discovery must be byte-identical to uncached");
}
