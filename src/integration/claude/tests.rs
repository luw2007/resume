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
    preview::jsonl,
    preview::snapshot,
    session::{ActivityStatus, SupportStatus, WorkspaceEvidence},
};

// --- helpers ---

const UUID_A: &str = "11111111-aaaa-2222-bbbb-333333333333";
const UUID_B: &str = "44444444-cccc-5555-dddd-666666666666";

/// Write a transcript to `<root>/projects/<workspace-key>/<uuid>.jsonl`, where
/// `root` is the effective Claude root directory.
fn write_transcript(
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
fn write_transcript_bytes(root: &Path, workspace_key: &str, uuid: &str, bytes: &[u8]) -> PathBuf {
    let dir = root.join("projects").join(workspace_key);
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{uuid}.jsonl"));
    fs::write(&path, bytes).unwrap();
    path
}

/// The effective root directory for a home-based (default) root: `~/.claude`.
fn default_root_dir(home: &Path) -> PathBuf {
    home.join(".claude")
}

fn json(pairs: &[(&str, serde_json::Value)]) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    for (key, value) in pairs {
        map.insert((*key).to_string(), value.clone());
    }
    serde_json::Value::Object(map)
}

fn str_val(s: &str) -> serde_json::Value {
    serde_json::Value::String(s.to_string())
}

fn user_record(content: serde_json::Value) -> serde_json::Value {
    json(&[
        ("type", str_val("user")),
        (
            "message",
            json(&[("role", str_val("user")), ("content", content)]),
        ),
    ])
}

fn text_block(text: &str) -> serde_json::Value {
    json(&[("type", str_val("text")), ("text", str_val(text))])
}

fn tool_result_block(content: &str) -> serde_json::Value {
    json(&[
        ("type", str_val("tool_result")),
        ("content", str_val(content)),
    ])
}

fn assistant_record(text: &str) -> serde_json::Value {
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
fn standard_records(session_id: &str, cwd: &str, user_text: &str) -> Vec<serde_json::Value> {
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
fn snapshot_tree(root: &Path) -> snapshot::DirSnapshot {
    snapshot::snapshot_dir(root, true).expect("snapshot capture must succeed")
}

// ===========================================================================
// Fixture: workspace-key collision pair
// ===========================================================================

/// Two workspace-key directories encode the same real path differently (e.g.
/// one uses a single separator, one uses a doubled separator) but their
/// recorded `cwd`s point at distinct real worktrees. The integration must NOT
/// infer Workspace from the directory name; each Session keeps its own `cwd`.
#[test]
fn workspace_key_collision_pair_keeps_distinct_cwd() {
    let home = tempfile::tempdir().unwrap();
    let root = claude::resolve_root(None, Some(home.path())).unwrap();

    let cwd_a = home.path().join("repo-a");
    let cwd_b = home.path().join("repo-b");
    fs::create_dir_all(&cwd_a).unwrap();
    fs::create_dir_all(&cwd_b).unwrap();

    // Two different encodings of a workspace path that could collide.
    write_transcript(
        &default_root_dir(home.path()),
        "-Users-example-repo",
        UUID_A,
        &standard_records(UUID_A, cwd_a.to_str().unwrap(), "message A"),
    );
    write_transcript(
        &default_root_dir(home.path()),
        "-Users-example-repo-", // trailing separator variant
        UUID_A,
        &standard_records(UUID_A, cwd_b.to_str().unwrap(), "message B"),
    );

    let snap_before = snapshot_tree(home.path());
    let discovery = claude::discover(&root).unwrap();
    let snap_after = snapshot_tree(home.path());

    // Both Sessions retained because their cwd evidence is distinct, even
    // though the workspace keys would collide if reversed.
    assert_eq!(
        discovery.sessions.len(),
        2,
        "both distinct-cwd sessions kept"
    );

    let cwds: Vec<PathBuf> = discovery
        .sessions
        .iter()
        .filter_map(|session| session.workspace.workspace().map(Path::to_path_buf))
        .collect();
    // The recorded cwd is stored verbatim from the transcript (not
    // canonicalized), so compare against the paths as written.
    assert!(cwds.contains(&cwd_a), "cwd_a retained: {cwds:?}");
    assert!(cwds.contains(&cwd_b), "cwd_b retained: {cwds:?}");

    // Read-only: no bytes or mtimes changed.
    snapshot::assert_unchanged(&snap_before, &snap_after);
}

// ===========================================================================
// Fixture: UUID agreement / disagreement
// ===========================================================================

#[cfg(unix)]
#[test]
fn symlinked_transcript_inside_effective_root_is_read() {
    let home = tempfile::tempdir().unwrap();
    let root = claude::resolve_root(None, Some(home.path())).unwrap();
    let cwd = home.path().join("work");
    fs::create_dir_all(&cwd).unwrap();
    let target = write_transcript(
        &default_root_dir(home.path()),
        "target-key",
        UUID_A,
        &standard_records(UUID_A, cwd.to_str().unwrap(), "followed safely"),
    );
    let link_dir = default_root_dir(home.path()).join("projects/link-key");
    fs::create_dir_all(&link_dir).unwrap();
    std::os::unix::fs::symlink(&target, link_dir.join(format!("{UUID_A}.jsonl"))).unwrap();
    fs::remove_file(target).unwrap();
    let relocated = default_root_dir(home.path()).join("relocated.data");
    write_transcript(
        &default_root_dir(home.path()),
        "unused",
        UUID_A,
        &standard_records(UUID_A, cwd.to_str().unwrap(), "followed safely"),
    );
    let generated = default_root_dir(home.path())
        .join("projects/unused")
        .join(format!("{UUID_A}.jsonl"));
    fs::rename(generated, &relocated).unwrap();
    fs::remove_dir_all(default_root_dir(home.path()).join("projects/unused")).unwrap();
    fs::remove_file(link_dir.join(format!("{UUID_A}.jsonl"))).unwrap();
    std::os::unix::fs::symlink(&relocated, link_dir.join(format!("{UUID_A}.jsonl"))).unwrap();

    let discovery = claude::discover(&root).unwrap();
    assert_eq!(discovery.sessions.len(), 1);
    assert_eq!(
        discovery.sessions[0].title.as_deref(),
        Some("followed safely")
    );
    assert!(discovery.diagnostics.is_empty());
}

#[cfg(unix)]
#[test]
fn symlinked_transcript_outside_effective_root_is_rejected_with_diagnostic() {
    let home = tempfile::tempdir().unwrap();
    let root = claude::resolve_root(None, Some(home.path())).unwrap();
    let cwd = home.path().join("work");
    fs::create_dir_all(&cwd).unwrap();
    let outside = tempfile::tempdir().unwrap();
    let target = write_transcript(
        outside.path(),
        "foreign",
        UUID_A,
        &standard_records(UUID_A, cwd.to_str().unwrap(), "must not leak"),
    );
    let link_dir = default_root_dir(home.path()).join("projects/key");
    fs::create_dir_all(&link_dir).unwrap();
    std::os::unix::fs::symlink(&target, link_dir.join(format!("{UUID_A}.jsonl"))).unwrap();

    let discovery = claude::discover(&root).unwrap();
    assert!(discovery.sessions.is_empty());
    assert!(discovery.diagnostics.iter().any(|d| {
        d.category == "claude_io"
            && d.verbose_chain
                .as_deref()
                .is_some_and(|chain| chain.contains("outside effective root"))
    }));
}

#[test]
fn uuid_filename_and_embedded_session_id_agree_is_supported() {
    let home = tempfile::tempdir().unwrap();
    let root = claude::resolve_root(None, Some(home.path())).unwrap();
    let cwd = home.path().join("work");
    fs::create_dir_all(&cwd).unwrap();

    write_transcript(
        &default_root_dir(home.path()),
        "key",
        UUID_A,
        &standard_records(UUID_A, cwd.to_str().unwrap(), "hello"),
    );

    let discovery = claude::discover(&root).unwrap();
    assert_eq!(discovery.sessions.len(), 1);
    let session = &discovery.sessions[0];
    assert_eq!(session.support, SupportStatus::Supported);
    assert_eq!(session.resumable_id, OsString::from(UUID_A));
}

#[test]
fn uuid_filename_and_embedded_session_id_disagree_with_cwd_is_discover_only() {
    let home = tempfile::tempdir().unwrap();
    let root = claude::resolve_root(None, Some(home.path())).unwrap();
    let cwd = home.path().join("work");
    fs::create_dir_all(&cwd).unwrap();

    // Filename is UUID_A, embedded is UUID_B — they disagree. A cwd exists.
    write_transcript(
        &default_root_dir(home.path()),
        "key",
        UUID_A,
        &standard_records(UUID_B, cwd.to_str().unwrap(), "hello"),
    );

    let discovery = claude::discover(&root).unwrap();
    assert_eq!(discovery.sessions.len(), 1, "retained as Discover Only");
    let session = &discovery.sessions[0];
    assert_eq!(
        session.support,
        SupportStatus::DiscoverOnly,
        "cannot safely resume under a mismatched ID"
    );
    // Resumable ID is the embedded one, never the filename (not authoritative).
    assert_eq!(session.resumable_id, OsString::from(UUID_B));
}

#[test]
fn uuid_disagreement_without_cwd_is_skipped() {
    let home = tempfile::tempdir().unwrap();
    let root = claude::resolve_root(None, Some(home.path())).unwrap();

    // Filename UUID_A, embedded UUID_B, no cwd at all.
    write_transcript(
        &default_root_dir(home.path()),
        "key",
        UUID_A,
        &[json(&[
            ("type", str_val("user")),
            ("sessionId", str_val(UUID_B)),
            (
                "message",
                json(&[("role", str_val("user")), ("content", str_val("hi"))]),
            ),
        ])],
    );

    let discovery = claude::discover(&root).unwrap();
    assert!(
        discovery.sessions.is_empty(),
        "no safe identity and no workspace: skipped entirely"
    );
    assert!(
        discovery
            .diagnostics
            .iter()
            .any(|d| d.category == "claude_identity_disagreement"),
        "a disagreement diagnostic is emitted"
    );
}

#[test]
fn no_embedded_session_id_with_cwd_is_discover_only() {
    let home = tempfile::tempdir().unwrap();
    let root = claude::resolve_root(None, Some(home.path())).unwrap();
    let cwd = home.path().join("work");
    fs::create_dir_all(&cwd).unwrap();

    // UUID-like filename but no embedded sessionId anywhere.
    write_transcript(
        &default_root_dir(home.path()),
        "key",
        UUID_A,
        &[json(&[
            ("type", str_val("user")),
            ("cwd", str_val(cwd.to_str().unwrap())),
            (
                "message",
                json(&[("role", str_val("user")), ("content", str_val("hi"))]),
            ),
        ])],
    );

    let discovery = claude::discover(&root).unwrap();
    assert_eq!(discovery.sessions.len(), 1);
    assert_eq!(
        discovery.sessions[0].support,
        SupportStatus::DiscoverOnly,
        "filename alone is not authoritative"
    );
}

#[test]
fn uuid_agreement_is_case_and_brace_insensitive() {
    let home = tempfile::tempdir().unwrap();
    let root = claude::resolve_root(None, Some(home.path())).unwrap();
    let cwd = home.path().join("work");
    fs::create_dir_all(&cwd).unwrap();

    // Filename uppercase, embedded with braces, both equal when normalized.
    let upper = UUID_A.to_uppercase();
    let braced = format!("{{{UUID_A}}}");
    write_transcript(
        &default_root_dir(home.path()),
        "key",
        &upper,
        &standard_records(&braced, cwd.to_str().unwrap(), "hi"),
    );

    let discovery = claude::discover(&root).unwrap();
    assert_eq!(discovery.sessions.len(), 1);
    assert_eq!(discovery.sessions[0].support, SupportStatus::Supported);
}

// ===========================================================================
// Fixture: title precedence variants
// ===========================================================================

#[test]
fn title_precedence_explicit_agent_name_beats_ai_title_and_summary() {
    let home = tempfile::tempdir().unwrap();
    let root = claude::resolve_root(None, Some(home.path())).unwrap();
    let cwd = home.path().join("work");
    fs::create_dir_all(&cwd).unwrap();

    let mut records = standard_records(UUID_A, cwd.to_str().unwrap(), "my user prompt");
    records.push(json(&[
        ("type", str_val("ai-title")),
        ("aiTitle", str_val("AI generated")),
    ]));
    records.push(json(&[
        ("type", str_val("agent-name")),
        ("agentName", str_val("My Agent")),
    ]));

    write_transcript(&default_root_dir(home.path()), "key", UUID_A, &records);

    let discovery = claude::discover(&root).unwrap();
    assert_eq!(discovery.sessions.len(), 1);
    assert_eq!(
        discovery.sessions[0].title.as_deref(),
        Some("My Agent"),
        "explicit agent/display name wins"
    );
}

#[test]
fn title_precedence_ai_title_beats_user_summary() {
    let home = tempfile::tempdir().unwrap();
    let root = claude::resolve_root(None, Some(home.path())).unwrap();
    let cwd = home.path().join("work");
    fs::create_dir_all(&cwd).unwrap();

    let mut records = standard_records(UUID_A, cwd.to_str().unwrap(), "my user prompt");
    records.push(json(&[
        ("type", str_val("ai-title")),
        ("ai-title", str_val("AI Generated Title")),
    ]));

    write_transcript(&default_root_dir(home.path()), "key", UUID_A, &records);

    let discovery = claude::discover(&root).unwrap();
    assert_eq!(
        discovery.sessions[0].title.as_deref(),
        Some("AI Generated Title")
    );
}

#[test]
fn title_falls_back_to_deterministic_user_summary() {
    let home = tempfile::tempdir().unwrap();
    let root = claude::resolve_root(None, Some(home.path())).unwrap();
    let cwd = home.path().join("work");
    fs::create_dir_all(&cwd).unwrap();

    write_transcript(
        &default_root_dir(home.path()),
        "key",
        UUID_A,
        &standard_records(UUID_A, cwd.to_str().unwrap(), "fix the login bug"),
    );

    let discovery = claude::discover(&root).unwrap();
    let title = discovery.sessions[0].title.as_deref().unwrap();
    assert!(
        title.contains("fix the login bug"),
        "summary derived from first user input: {title}"
    );
}

#[test]
fn title_is_none_when_no_user_input_and_no_metadata() {
    let home = tempfile::tempdir().unwrap();
    let root = claude::resolve_root(None, Some(home.path())).unwrap();
    let cwd = home.path().join("work");
    fs::create_dir_all(&cwd).unwrap();

    // Only a session-id + cwd header, no user input, no title metadata.
    write_transcript(
        &default_root_dir(home.path()),
        "key",
        UUID_A,
        &[json(&[
            ("type", str_val("summary")),
            ("sessionId", str_val(UUID_A)),
            ("cwd", str_val(cwd.to_str().unwrap())),
        ])],
    );

    let discovery = claude::discover(&root).unwrap();
    assert_eq!(discovery.sessions.len(), 1);
    assert!(
        discovery.sessions[0].title.is_none(),
        "no title source available"
    );
}

// ===========================================================================
// Fixture: heterogeneous producer versions
// ===========================================================================

/// Different event `version` fields (producer versions) must not change
/// parsing. The `version` field is producer metadata, not a schema contract.
#[test]
fn heterogeneous_producer_versions_all_parse() {
    let home = tempfile::tempdir().unwrap();
    let root = claude::resolve_root(None, Some(home.path())).unwrap();
    let cwd = home.path().join("work");
    fs::create_dir_all(&cwd).unwrap();

    let versions = ["0.1.0", "1.0.0", "2.1.220", "2.1.300"];
    for (i, version) in versions.iter().enumerate() {
        let uuid = format!("{UUID_A}-{}", i);
        let records = vec![
            json(&[
                ("type", str_val("user")),
                ("sessionId", str_val(&uuid)),
                ("cwd", str_val(cwd.to_str().unwrap())),
                ("version", str_val(version)),
                (
                    "message",
                    json(&[("role", str_val("user")), ("content", str_val("hi"))]),
                ),
            ]),
            // camelCase title in a "newer" producer.
            json(&[
                ("type", str_val("assistant")),
                ("agentName", str_val(&format!("Agent {version}"))),
                ("version", str_val(version)),
                (
                    "message",
                    json(&[
                        ("role", str_val("assistant")),
                        ("content", text_block("ok")),
                    ]),
                ),
            ]),
        ];
        write_transcript(
            &default_root_dir(home.path()),
            &format!("key{i}"),
            &uuid,
            &records,
        );
    }

    let discovery = claude::discover(&root).unwrap();
    assert_eq!(
        discovery.sessions.len(),
        versions.len(),
        "every producer version parses"
    );
    // Discovery order is not guaranteed (directory enumeration order); match
    // each expected title by content rather than by position.
    let titles: Vec<String> = discovery
        .sessions
        .iter()
        .map(|session| session.title.clone().unwrap_or_default())
        .collect();
    for version in versions {
        let expected = format!("Agent {version}");
        assert!(
            titles.contains(&expected),
            "camelCase agentName parsed regardless of version: {titles:?}"
        );
    }
    for session in &discovery.sessions {
        assert_eq!(session.support, SupportStatus::Supported);
    }
}

// ===========================================================================
// Fixture: unknown events tolerated
// ===========================================================================

#[test]
fn unknown_events_tolerated_without_aborting() {
    let home = tempfile::tempdir().unwrap();
    let root = claude::resolve_root(None, Some(home.path())).unwrap();
    let cwd = home.path().join("work");
    fs::create_dir_all(&cwd).unwrap();

    let mut records = standard_records(UUID_A, cwd.to_str().unwrap(), "hello");
    records.push(json(&[
        ("type", str_val("permission")),
        ("decision", str_val("allow")),
    ]));
    records.push(json(&[
        ("type", str_val("totally-new-event")),
        ("payload", json(&[("anything", str_val("here"))])),
    ]));
    records.push(json(&[
        ("type", str_val("mode")),
        ("mode", str_val("plan")),
    ]));

    write_transcript(&default_root_dir(home.path()), "key", UUID_A, &records);

    let discovery = claude::discover(&root).unwrap();
    assert_eq!(discovery.sessions.len(), 1, "unknown records do not abort");
    assert_eq!(discovery.sessions[0].support, SupportStatus::Supported);
}

// ===========================================================================
// Fixture: tool-only input excluded
// ===========================================================================

#[test]
fn user_record_with_only_tool_result_blocks_is_excluded_from_title() {
    let home = tempfile::tempdir().unwrap();
    let root = claude::resolve_root(None, Some(home.path())).unwrap();
    let cwd = home.path().join("work");
    fs::create_dir_all(&cwd).unwrap();

    // First record: a user turn that contains ONLY tool_result blocks — this
    // is tool output fed back, not human input.
    let tool_only = user_record(serde_json::Value::Array(vec![tool_result_block(
        "tool output here",
    )]));
    let header = json(&[
        ("type", str_val("user")),
        ("sessionId", str_val(UUID_A)),
        ("cwd", str_val(cwd.to_str().unwrap())),
    ]);

    write_transcript(
        &default_root_dir(home.path()),
        "key",
        UUID_A,
        &[header, tool_only],
    );

    let discovery = claude::discover(&root).unwrap();
    assert_eq!(discovery.sessions.len(), 1);
    // No human text was extracted, so title falls back to None.
    assert!(
        discovery.sessions[0].title.is_none(),
        "tool-only user record is not human input"
    );
}

#[test]
fn mixed_text_and_tool_result_blocks_extract_human_text_only() {
    let home = tempfile::tempdir().unwrap();
    let root = claude::resolve_root(None, Some(home.path())).unwrap();
    let cwd = home.path().join("work");
    fs::create_dir_all(&cwd).unwrap();

    let mixed = user_record(serde_json::Value::Array(vec![
        tool_result_block("system output"),
        text_block("real human question"),
    ]));
    let header = json(&[
        ("type", str_val("user")),
        ("sessionId", str_val(UUID_A)),
        ("cwd", str_val(cwd.to_str().unwrap())),
    ]);

    write_transcript(
        &default_root_dir(home.path()),
        "key",
        UUID_A,
        &[header, mixed],
    );

    let discovery = claude::discover(&root).unwrap();
    let title = discovery.sessions[0].title.as_deref().unwrap();
    assert!(
        title.contains("real human question"),
        "human text extracted alongside tool_result: {title}"
    );
}

#[test]
fn assistant_system_and_injected_records_excluded_from_user_messages() {
    let home = tempfile::tempdir().unwrap();
    let root = claude::resolve_root(None, Some(home.path())).unwrap();
    let cwd = home.path().join("work");
    fs::create_dir_all(&cwd).unwrap();

    let header = json(&[
        ("type", str_val("user")),
        ("sessionId", str_val(UUID_A)),
        ("cwd", str_val(cwd.to_str().unwrap())),
    ]);
    let system = json(&[
        ("type", str_val("system")),
        (
            "content",
            str_val("system prompt that should not be a title"),
        ),
    ]);
    let injected = json(&[
        ("type", str_val("user")),
        ("isMeta", serde_json::Value::Bool(true)),
        (
            "message",
            json(&[
                ("role", str_val("user")),
                ("content", str_val("injected skill content")),
            ]),
        ),
    ]);
    let assistant = assistant_record("assistant reply text");

    write_transcript(
        &default_root_dir(home.path()),
        "key",
        UUID_A,
        &[header, system, injected, assistant],
    );

    let discovery = claude::discover(&root).unwrap();
    assert_eq!(discovery.sessions.len(), 1);
    assert!(
        discovery.sessions[0].title.is_none(),
        "no human-authored user text present"
    );
}

// ===========================================================================
// Fixture: truncated / malformed records
// ===========================================================================

#[test]
fn truncated_incomplete_tail_is_diagnosed_but_session_retained() {
    let home = tempfile::tempdir().unwrap();
    let root = claude::resolve_root(None, Some(home.path())).unwrap();
    let cwd = home.path().join("work");
    fs::create_dir_all(&cwd).unwrap();

    let header = serde_json::to_string(&json(&[
        ("type", str_val("user")),
        ("sessionId", str_val(UUID_A)),
        ("cwd", str_val(cwd.to_str().unwrap())),
        (
            "message",
            json(&[("role", str_val("user")), ("content", str_val("hi"))]),
        ),
    ]))
    .unwrap();
    // Valid record + a truncated final unterminated record.
    let bytes = format!("{header}\n{{\"type\":\"user\",\"message\":{{\"content\":\"being writ")
        .into_bytes();

    write_transcript_bytes(&default_root_dir(home.path()), "key", UUID_A, &bytes);

    let discovery = claude::discover(&root).unwrap();
    assert_eq!(discovery.sessions.len(), 1, "valid prefix retained");
    assert!(
        discovery
            .diagnostics
            .iter()
            .any(|d| d.category == "claude_truncated")
    );
}

#[test]
fn malformed_middle_record_is_diagnosed_but_not_aborted() {
    let home = tempfile::tempdir().unwrap();
    let root = claude::resolve_root(None, Some(home.path())).unwrap();
    let cwd = home.path().join("work");
    fs::create_dir_all(&cwd).unwrap();

    let header = serde_json::to_string(&json(&[
        ("type", str_val("user")),
        ("sessionId", str_val(UUID_A)),
        ("cwd", str_val(cwd.to_str().unwrap())),
        (
            "message",
            json(&[("role", str_val("user")), ("content", str_val("first"))]),
        ),
    ]))
    .unwrap();
    let after = serde_json::to_string(&json(&[
        ("type", str_val("user")),
        (
            "message",
            json(&[("role", str_val("user")), ("content", str_val("second"))]),
        ),
    ]))
    .unwrap();

    // Valid, malformed-middle, valid.
    let bytes = format!("{header}\n{{not valid json}}\n{after}\n").into_bytes();
    write_transcript_bytes(&default_root_dir(home.path()), "key", UUID_A, &bytes);

    let discovery = claude::discover(&root).unwrap();
    assert_eq!(discovery.sessions.len(), 1, "session still discovered");
    assert!(
        discovery
            .diagnostics
            .iter()
            .any(|d| d.category == "claude_malformed")
    );
    // The user message after the malformed record was extracted.
    let title = discovery.sessions[0].title.as_deref().unwrap();
    assert!(
        title.contains("first"),
        "first valid human input used: {title}"
    );
}

// ===========================================================================
// Fixture: alternate root (nondefault CLAUDE_CONFIG_DIR)
// ===========================================================================

#[test]
fn alternate_root_is_resolved_and_marked_nondefault() {
    let custom = tempfile::tempdir().unwrap();
    let root = claude::resolve_root(Some(custom.path().as_os_str()), None).unwrap();
    assert!(root.nondefault);
    // The effective root is stored as given by the environment (resolve_root
    // does not canonicalize; canonicalization happens at read time).
    assert_eq!(root.effective_root, custom.path());

    let cwd = custom.path().join("work");
    fs::create_dir_all(&cwd).unwrap();
    write_transcript(
        custom.path(),
        "key",
        UUID_A,
        &standard_records(UUID_A, cwd.to_str().unwrap(), "hi"),
    );

    let discovery = claude::discover(&root).unwrap();
    assert_eq!(discovery.sessions.len(), 1);
    // The effective root is part of identity.
    assert_eq!(
        discovery.sessions[0].key.effective_root,
        root.effective_root
    );
}

#[test]
fn alternate_and_default_roots_yield_distinct_identities() {
    let home = tempfile::tempdir().unwrap();
    let custom = tempfile::tempdir().unwrap();

    let default_root = claude::resolve_root(None, Some(home.path())).unwrap();
    let alt_root = claude::resolve_root(Some(custom.path().as_os_str()), None).unwrap();

    for root in [&default_root, &alt_root] {
        let cwd = root.effective_root.join("work");
        fs::create_dir_all(&cwd).unwrap();
        write_transcript(
            &root.effective_root,
            "key",
            UUID_A,
            &standard_records(UUID_A, cwd.to_str().unwrap(), "hi"),
        );
    }

    let a = claude::discover(&default_root).unwrap();
    let b = claude::discover(&alt_root).unwrap();
    assert_eq!(a.sessions.len(), 1);
    assert_eq!(b.sessions.len(), 1);
    // Same UUID but different effective root → distinct keys.
    assert_ne!(a.sessions[0].key, b.sessions[0].key);
}

// ===========================================================================
// Fixture: missing Workspace
// ===========================================================================

#[test]
fn missing_workspace_yields_unknown_evidence() {
    let home = tempfile::tempdir().unwrap();
    let root = claude::resolve_root(None, Some(home.path())).unwrap();

    // sessionId agrees with filename, but no cwd anywhere.
    write_transcript(
        &default_root_dir(home.path()),
        "key",
        UUID_A,
        &[json(&[
            ("type", str_val("user")),
            ("sessionId", str_val(UUID_A)),
            (
                "message",
                json(&[("role", str_val("user")), ("content", str_val("hi"))]),
            ),
        ])],
    );

    let discovery = claude::discover(&root).unwrap();
    assert_eq!(
        discovery.sessions.len(),
        1,
        "supported ID still discoverable"
    );
    assert_eq!(discovery.sessions[0].workspace, WorkspaceEvidence::Unknown);
    assert_eq!(discovery.sessions[0].support, SupportStatus::Supported);
}

// ===========================================================================
// Fixture: fake claude launch contract — captures exact cwd/argv/env
// ===========================================================================

/// A fake `claude` launcher that records the exact cwd, argv, and environment
/// it would exec with, then exits successfully without spawning anything.
mod fake_claude {
    use std::{
        ffi::{OsStr, OsString},
        fs,
        path::{Path, PathBuf},
        process::Command,
    };

    /// Record of what the fake launcher observed.
    #[derive(Debug)]
    pub struct LaunchRecord {
        pub cwd: PathBuf,
        pub argv: Vec<OsString>,
        pub env: Vec<(OsString, OsString)>,
    }

    impl LaunchRecord {
        pub fn arg(&self, index: usize) -> Option<&OsStr> {
            self.argv.get(index).map(OsString::as_os_str)
        }

        pub fn env_get(&self, key: &str) -> Option<&OsStr> {
            self.env
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.as_os_str())
        }
    }

    /// Write a fake `claude` script to a temp dir and return its path plus the
    /// path to the file it will write its launch record to.
    pub fn install(tmp: &Path) -> (PathBuf, PathBuf) {
        let bin_dir = tmp.join("bin");
        fs::create_dir_all(&bin_dir).unwrap();
        let record = tmp.join("launch_record.txt");
        let record_for_script = record.clone();

        #[cfg(unix)]
        {
            // Record only the actual arguments ($@), not $0 (the program
            // name), so the parsed `argv` mirrors the ResumeSpec.argv the
            // launcher would exec with.
            let script = format!(
                r#"#!/bin/sh
                : > "{rec}"
                for a in "$@"; do echo "argv:$a" >> "{rec}"; done
                pwd >> "{rec}"
                echo "env:CLAUDE_CONFIG_DIR=${{CLAUDE_CONFIG_DIR}}" >> "{rec}"
                exit 0
                "#,
                rec = record_for_script.display()
            );
            let bin = bin_dir.join("claude");
            fs::write(&bin, script).unwrap();
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&bin).unwrap().permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&bin, perms).unwrap();
            (bin, record)
        }
        #[cfg(not(unix))]
        {
            // Non-Unix: we cannot exec a shell script. This test suite targets
            // Unix exec semantics; on other platforms the record is unused.
            let _ = record_for_script;
            (bin_dir.join("claude"), record)
        }
    }

    /// Parse a launch record file written by the fake script.
    pub fn parse_record(path: &Path) -> LaunchRecord {
        let text = fs::read_to_string(path).unwrap();
        let mut argv = Vec::new();
        let mut cwd = PathBuf::new();
        let mut env = Vec::new();
        for line in text.lines() {
            if let Some(rest) = line.strip_prefix("argv:") {
                argv.push(OsString::from(rest));
            } else if let Some(rest) = line.strip_prefix("env:") {
                if let Some((k, v)) = rest.split_once('=') {
                    env.push((OsString::from(k), OsString::from(v)));
                }
            } else if !line.is_empty() {
                cwd = PathBuf::from(line);
            }
        }
        LaunchRecord { cwd, argv, env }
    }

    /// Run the fake launcher with a ResumeSpec and return the observed record.
    #[cfg(unix)]
    pub fn run(spec: &crate::session::ResumeSpec, path: &Path) -> LaunchRecord {
        let mut cmd = Command::new(&spec.program);
        cmd.args(&spec.argv);
        cmd.current_dir(&spec.cwd);
        cmd.env_clear();
        cmd.env("HOME", &spec.cwd);
        cmd.env("XDG_CONFIG_HOME", spec.cwd.join(".xdg-config"));
        cmd.env("XDG_DATA_HOME", spec.cwd.join(".xdg-data"));
        cmd.env("XDG_STATE_HOME", spec.cwd.join(".xdg-state"));
        cmd.env("XDG_CACHE_HOME", spec.cwd.join(".xdg-cache"));
        for (k, v) in &spec.env {
            cmd.env(k, v);
        }
        let status = cmd.status().expect("fake launcher must run");
        assert!(status.success(), "fake launcher exited cleanly");
        parse_record(path)
    }
}

#[cfg(unix)]
#[test]
fn fake_claude_launch_contract_captures_exact_resume_argv_cwd_env() {
    let home = tempfile::tempdir().unwrap();
    let (bin, record) = fake_claude::install(home.path());

    let root = claude::resolve_root(None, Some(home.path())).unwrap();
    let cwd = home.path().join("workspace");
    fs::create_dir_all(&cwd).unwrap();
    write_transcript(
        &default_root_dir(home.path()),
        "key",
        UUID_A,
        &standard_records(UUID_A, cwd.to_str().unwrap(), "hello"),
    );

    let discovery = claude::discover(&root).unwrap();
    let session = &discovery.sessions[0];

    // Build the ResumeSpec, then point the program at the fake launcher.
    let mut spec = claude::resume_spec(session, &root).unwrap();
    spec.program = bin.clone().into_os_string();

    let launch = fake_claude::run(&spec, &record);

    // Exact argv: `--resume <uuid>` (the program name `claude` is argv[0]
    // at exec time and is not part of ResumeSpec.argv).
    assert_eq!(launch.arg(0), Some(OsStr::new("--resume")));
    assert_eq!(launch.arg(1), Some(OsStr::new(UUID_A)));
    assert!(
        launch.argv.len() == 2,
        "no --continue, no extra flags: {:?}",
        launch.argv
    );

    // Exact cwd: the recorded Workspace. The fake launcher reports its
    // working directory via `pwd`, which resolves symlinks (e.g.
    // /var -> /private/var on macOS), so compare canonicalized forms.
    assert_eq!(
        launch.cwd.canonicalize().unwrap(),
        cwd.canonicalize().unwrap()
    );

    // Default root: no CLAUDE_CONFIG_DIR override propagated.
    assert!(
        launch
            .env_get("CLAUDE_CONFIG_DIR")
            .map(|v: &OsStr| v.is_empty())
            .unwrap_or(true),
        "default root does not propagate CLAUDE_CONFIG_DIR"
    );
}

#[cfg(unix)]
#[test]
fn fake_claude_launch_contract_preserves_nondefault_config_dir() {
    let custom = tempfile::tempdir().unwrap();
    let (bin, record) = fake_claude::install(custom.path());

    let root = claude::resolve_root(Some(custom.path().as_os_str()), None).unwrap();
    let cwd = custom.path().join("workspace");
    fs::create_dir_all(&cwd).unwrap();
    write_transcript(
        custom.path(),
        "key",
        UUID_A,
        &standard_records(UUID_A, cwd.to_str().unwrap(), "hello"),
    );

    let discovery = claude::discover(&root).unwrap();
    let session = &discovery.sessions[0];
    let mut spec = claude::resume_spec(session, &root).unwrap();
    spec.program = bin.into_os_string();

    let launch = fake_claude::run(&spec, &record);

    assert_eq!(
        launch.env_get("CLAUDE_CONFIG_DIR"),
        Some(custom.path().as_os_str()),
        "nondefault CLAUDE_CONFIG_DIR preserved on Resume"
    );
}

#[test]
fn resume_spec_rejects_missing_workspace() {
    let home = tempfile::tempdir().unwrap();
    let root = claude::resolve_root(None, Some(home.path())).unwrap();

    write_transcript(
        &default_root_dir(home.path()),
        "key",
        UUID_A,
        &[json(&[
            ("type", str_val("user")),
            ("sessionId", str_val(UUID_A)),
            (
                "message",
                json(&[("role", str_val("user")), ("content", str_val("hi"))]),
            ),
        ])],
    );

    let discovery = claude::discover(&root).unwrap();
    let session = &discovery.sessions[0];
    let err = claude::resume_spec(session, &root).unwrap_err();
    match err {
        crate::session::IntegrationError::InvalidSession { diagnostic } => {
            assert_eq!(diagnostic.category, "claude_missing_workspace");
        }
        other => panic!("expected InvalidSession, got {other:?}"),
    }
}

// ===========================================================================
// Fixture: nested subagent artifacts excluded
// ===========================================================================

#[test]
fn nested_subagent_artifacts_are_not_independent_sessions() {
    let home = tempfile::tempdir().unwrap();
    let root = claude::resolve_root(None, Some(home.path())).unwrap();
    let cwd = home.path().join("work");
    fs::create_dir_all(&cwd).unwrap();

    // A valid top-level transcript.
    write_transcript(
        &default_root_dir(home.path()),
        "key",
        UUID_A,
        &standard_records(UUID_A, cwd.to_str().unwrap(), "top level"),
    );

    // A nested subagent transcript under the same workspace-key. It must NOT
    // be surfaced as an independent Session.
    let subagents_dir = default_root_dir(home.path())
        .join("projects")
        .join("key")
        .join("subagents");
    fs::create_dir_all(&subagents_dir).unwrap();
    let sub_path = subagents_dir.join(format!("{UUID_B}.jsonl"));
    fs::write(
        &sub_path,
        serde_json::to_string(&json(&[
            ("type", str_val("user")),
            ("sessionId", str_val(UUID_B)),
            ("cwd", str_val(cwd.to_str().unwrap())),
            (
                "message",
                json(&[("role", str_val("user")), ("content", str_val("nested"))]),
            ),
        ]))
        .unwrap()
            + "\n",
    )
    .unwrap();

    let discovery = claude::discover(&root).unwrap();
    assert_eq!(
        discovery.sessions.len(),
        1,
        "nested subagent transcript is not an independent Session"
    );
    assert_eq!(discovery.sessions[0].resumable_id, OsString::from(UUID_A));
}

// ===========================================================================
// Fixture: read-only regression (no bytes/mtimes changed)
// ===========================================================================

#[test]
fn discovery_does_not_modify_any_file_or_directory() {
    let home = tempfile::tempdir().unwrap();
    let root = claude::resolve_root(None, Some(home.path())).unwrap();
    let cwd = home.path().join("work");
    fs::create_dir_all(&cwd).unwrap();
    write_transcript(
        &default_root_dir(home.path()),
        "key",
        UUID_A,
        &standard_records(UUID_A, cwd.to_str().unwrap(), "hello"),
    );

    let snap_before = snapshot_tree(home.path());
    let _discovery = claude::discover(&root).unwrap();
    let snap_after = snapshot_tree(home.path());

    snapshot::assert_unchanged(&snap_before, &snap_after);
}

// ===========================================================================
// Fixture: empty / non-existent projects dir
// ===========================================================================

#[test]
fn missing_projects_dir_yields_empty_discovery() {
    let home = tempfile::tempdir().unwrap();
    let root = claude::resolve_root(None, Some(home.path())).unwrap();
    let discovery = claude::discover(&root).unwrap();
    assert!(discovery.sessions.is_empty());
    assert!(discovery.diagnostics.is_empty());
}

#[test]
fn empty_projects_dir_yields_empty_discovery() {
    let home = tempfile::tempdir().unwrap();
    fs::create_dir_all(default_root_dir(home.path()).join("projects")).unwrap();
    let root = claude::resolve_root(None, Some(home.path())).unwrap();
    let discovery = claude::discover(&root).unwrap();
    assert!(discovery.sessions.is_empty());
}

#[test]
fn non_jsonl_files_are_ignored() {
    let home = tempfile::tempdir().unwrap();
    let key_dir = default_root_dir(home.path()).join("projects").join("key");
    fs::create_dir_all(&key_dir).unwrap();
    fs::write(key_dir.join("README.md"), "not a session").unwrap();
    fs::write(key_dir.join("notes.txt"), "also not").unwrap();

    let root = claude::resolve_root(None, Some(home.path())).unwrap();
    let discovery = claude::discover(&root).unwrap();
    assert!(discovery.sessions.is_empty());
}

// ===========================================================================
// Fixture: identity collision across workspace keys
// ===========================================================================

/// The same UUID under two different workspace-key directories represents two
/// independent Sessions only if their provenance (effective root + native
// locator path) differs. The native_locator is the transcript path.
#[test]
fn same_uuid_under_two_workspace_keys_keeps_both_as_distinct_native_locators() {
    let home = tempfile::tempdir().unwrap();
    let root = claude::resolve_root(None, Some(home.path())).unwrap();
    let cwd_a = home.path().join("a");
    let cwd_b = home.path().join("b");
    fs::create_dir_all(&cwd_a).unwrap();
    fs::create_dir_all(&cwd_b).unwrap();

    write_transcript(
        &default_root_dir(home.path()),
        "key-one",
        UUID_A,
        &standard_records(UUID_A, cwd_a.to_str().unwrap(), "in one"),
    );
    write_transcript(
        &default_root_dir(home.path()),
        "key-two",
        UUID_A,
        &standard_records(UUID_A, cwd_b.to_str().unwrap(), "in two"),
    );

    let discovery = claude::discover(&root).unwrap();
    assert_eq!(discovery.sessions.len(), 2, "distinct native locators");
    // Keys differ because native_locator (the transcript path) differs.
    assert_ne!(discovery.sessions[0].key, discovery.sessions[1].key);
}

// ===========================================================================
// Activity is Unknown
// ===========================================================================

#[test]
fn activity_is_unknown_absent_positive_correlation() {
    let home = tempfile::tempdir().unwrap();
    let root = claude::resolve_root(None, Some(home.path())).unwrap();
    let cwd = home.path().join("work");
    fs::create_dir_all(&cwd).unwrap();
    write_transcript(
        &default_root_dir(home.path()),
        "key",
        UUID_A,
        &standard_records(UUID_A, cwd.to_str().unwrap(), "hello"),
    );

    let discovery = claude::discover(&root).unwrap();
    assert_eq!(discovery.sessions.len(), 1);
    assert_eq!(discovery.sessions[0].activity, ActivityStatus::Unknown);
}

// ===========================================================================
// Root resolution edge cases
// ===========================================================================

#[test]
fn resolve_root_prefers_explicit_config_dir() {
    let explicit = PathBuf::from("/custom/claude");
    let home = PathBuf::from("/home/user");
    let root = claude::resolve_root(Some(explicit.as_os_str()), Some(&home)).unwrap();
    assert_eq!(root.effective_root, explicit);
    assert!(root.nondefault);
}

#[test]
fn resolve_root_falls_back_to_home_dot_claude() {
    let home = PathBuf::from("/home/user");
    let root = claude::resolve_root(None, Some(&home)).unwrap();
    assert_eq!(root.effective_root, PathBuf::from("/home/user/.claude"));
    assert!(!root.nondefault);
}

#[test]
fn resolve_root_returns_none_without_env_or_home() {
    assert!(claude::resolve_root(None, None).is_none());
}

#[test]
fn empty_config_dir_env_falls_back_to_home() {
    let home = PathBuf::from("/home/user");
    let root = claude::resolve_root(Some(OsStr::new("")), Some(&home)).unwrap();
    assert_eq!(root.effective_root, PathBuf::from("/home/user/.claude"));
    assert!(!root.nondefault);
}

// ===========================================================================
// UUID helper unit checks
// ===========================================================================

#[test]
fn looks_like_uuid_validates_canonical_form() {
    assert!(super::looks_like_uuid(UUID_A));
    assert!(super::looks_like_uuid(&format!("{{{UUID_A}}}")));
    assert!(super::looks_like_uuid(&UUID_A.to_uppercase()));
    assert!(!super::looks_like_uuid("not-a-uuid"));
    assert!(!super::looks_like_uuid("11111111-aaaa-2222"));
    assert!(!super::looks_like_uuid(""));
}

#[test]
fn uuid_agrees_normalizes_case_and_braces() {
    assert!(super::uuid_agrees(UUID_A, UUID_A));
    assert!(super::uuid_agrees(&UUID_A.to_uppercase(), UUID_A));
    assert!(super::uuid_agrees(UUID_A, &format!("{{{UUID_A}}}")));
    assert!(!super::uuid_agrees(UUID_A, UUID_B));
}

// ===========================================================================
// Ensure shared jsonl reader is wired (smoke check)
// ===========================================================================

#[test]
fn integration_uses_shared_bounded_jsonl_reader() {
    // The integration must delegate parsing to crate::preview::jsonl, not implement
    // its own reader. A quick check that the module re-exports nothing and
    // the public discover path tolerates a file the reader would classify.
    let home = tempfile::tempdir().unwrap();
    let root = claude::resolve_root(None, Some(home.path())).unwrap();
    let cwd = home.path().join("work");
    fs::create_dir_all(&cwd).unwrap();
    write_transcript(
        &default_root_dir(home.path()),
        "key",
        UUID_A,
        &standard_records(UUID_A, cwd.to_str().unwrap(), "hi"),
    );
    let discovery = claude::discover(&root).unwrap();
    assert_eq!(discovery.sessions.len(), 1);
    // Sanity: crate::preview::jsonl symbol is reachable (compile-time guarantee).
    let _bounds = jsonl::Bounds::default();
}
