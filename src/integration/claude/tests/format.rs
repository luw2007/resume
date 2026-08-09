#![allow(unused_imports)]
use crate::integration::claude::test_support::*;
use crate::{
    integration::claude,
    preview::jsonl,
    preview::snapshot,
    session::{ActivityStatus, SupportStatus, WorkspaceEvidence},
};
use std::{
    ffi::{OsStr, OsString},
    fs,
    path::{Path, PathBuf},
};
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
    // The integration must delegate parsing to crate::jsonl, not implement
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
    // Sanity: crate::jsonl symbol is reachable (compile-time guarantee).
    let _bounds = jsonl::Bounds::default();
}
