#![allow(unused_imports)]
//! Tests for the Claude Code integration.
//!
//! Covers every fixture required by `plans/v0.1.0-implementation.md` Step 5:
//! workspace-key collisions, UUID agreement/disagreement, title variants,
//! heterogeneous producer versions, unknown events, tool-only input,
//! truncated/malformed records, alternate root, missing Workspace, and a
//! fake `claude` launch contract that captures exact cwd/argv/env.
//!
//! All tests run against temporary fixtures. No test reads a real `~/.claude`
//! or invokes the real Claude CLI. Read-only snapshots are asserted where the
//! plan requires them.

use std::{
    ffi::{OsStr, OsString},
    fs,
    path::{Path, PathBuf},
};

use crate::{
    integration::claude,
    jsonl,
    session::{ActivityStatus, SupportStatus, WorkspaceEvidence},
    snapshot,
};

// --- helpers ---

pub(crate) const UUID_A: &str = "11111111-aaaa-2222-bbbb-333333333333";
pub(crate) const UUID_B: &str = "44444444-cccc-5555-dddd-666666666666";

/// Write a transcript to `<root>/projects/<workspace-key>/<uuid>.jsonl`, where
/// `root` is the effective Claude root directory.
pub(crate) fn write_transcript(
    root: &Path,
    workspace_key: &str,
    uuid: &str,
    records: &[serde_json::Value],
) -> PathBuf {
    let dir = root.join("projects").join(workspace_key);
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{uuid}.jsonl"));
    let mut body = String::new();
    for record in records {
        body.push_str(&serde_json::to_string(record).unwrap());
        body.push('\n');
    }
    fs::write(&path, body).unwrap();
    path
}

/// Write raw bytes for a transcript (for truncation/malformed tests).
pub(crate) fn write_transcript_bytes(
    root: &Path,
    workspace_key: &str,
    uuid: &str,
    bytes: &[u8],
) -> PathBuf {
    let dir = root.join("projects").join(workspace_key);
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{uuid}.jsonl"));
    fs::write(&path, bytes).unwrap();
    path
}

/// The effective root directory for a home-based (default) root: `~/.claude`.
pub(crate) fn default_root_dir(home: &Path) -> PathBuf {
    home.join(".claude")
}

pub(crate) fn json(pairs: &[(&str, serde_json::Value)]) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    for (key, value) in pairs {
        map.insert((*key).to_string(), value.clone());
    }
    serde_json::Value::Object(map)
}

pub(crate) fn str_val(s: &str) -> serde_json::Value {
    serde_json::Value::String(s.to_string())
}

pub(crate) fn user_record(content: serde_json::Value) -> serde_json::Value {
    json(&[
        ("type", str_val("user")),
        (
            "message",
            json(&[("role", str_val("user")), ("content", content)]),
        ),
    ])
}

pub(crate) fn text_block(text: &str) -> serde_json::Value {
    json(&[("type", str_val("text")), ("text", str_val(text))])
}

pub(crate) fn tool_result_block(content: &str) -> serde_json::Value {
    json(&[
        ("type", str_val("tool_result")),
        ("content", str_val(content)),
    ])
}

pub(crate) fn assistant_record(text: &str) -> serde_json::Value {
    json(&[
        ("type", str_val("assistant")),
        (
            "message",
            json(&[
                ("role", str_val("assistant")),
                ("content", text_block(text)),
            ]),
        ),
    ])
}

/// A standard valid transcript with a session-id header, cwd, and one user msg.
pub(crate) fn standard_records(
    session_id: &str,
    cwd: &str,
    user_text: &str,
) -> Vec<serde_json::Value> {
    vec![json(&[
        ("type", str_val("user")),
        ("sessionId", str_val(session_id)),
        ("cwd", str_val(cwd)),
        (
            "message",
            json(&[("role", str_val("user")), ("content", str_val(user_text))]),
        ),
    ])]
}

/// Snapshot a directory tree's bytes and mtimes, for read-only regression.
pub(crate) fn snapshot_tree(root: &Path) -> snapshot::DirSnapshot {
    snapshot::snapshot_dir(root, true).expect("snapshot capture must succeed")
}
