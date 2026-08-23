use super::*;

// ===========================================================================
// HEADER PARSING: title-before-header (do NOT reuse Pi assumptions)
// ===========================================================================

#[test]
fn parses_v3_header_when_title_sidecar_precedes_it() {
    let fx = Fixture::new();
    fx.write(
        &fx.default_agent_root,
        &fx.encoded_ws(),
        "s.jsonl",
        &[
            title_sidecar("Initial Title"),
            header_v3("abc-123", &fx.workspace, 1700000000),
            user_message_string("hello world", 1700000010),
        ],
    );
    let outcome = fx.discover(fx.roots_default());
    assert_eq!(outcome.parsed.len(), 1);
    let parsed = &outcome.parsed[0];
    assert_eq!(parsed.id, "abc-123");
    assert_eq!(parsed.workspace.as_ref().unwrap(), &fx.workspace);
    assert_eq!(parsed.messages.len(), 1);
    assert_eq!(parsed.messages[0].text, "hello world");
}

#[test]
fn file_without_session_header_skipped_as_no_header() {
    let fx = Fixture::new();
    // Title sidecar present but no session header.
    fx.write(
        &fx.default_agent_root,
        &fx.encoded_ws(),
        "nohdr.jsonl",
        &[
            title_sidecar("Only Title"),
            user_message_string("hello", 1700000000),
        ],
    );
    let outcome = fx.discover(fx.roots_default());
    assert_eq!(outcome.parsed.len(), 0);
    assert_eq!(outcome.no_header_files, 1);
}

#[test]
fn header_not_required_to_be_first_record() {
    // A title record, then an unknown record, then the header, then a user
    // message. The header must still be found.
    let fx = Fixture::new();
    let unknown = json!({ "type": "unknown_future_record", "foo": "bar" });
    fx.write(
        &fx.default_agent_root,
        &fx.encoded_ws(),
        "order.jsonl",
        &[
            title_sidecar("T"),
            unknown,
            header_v3("late-hdr", &fx.workspace, 1700000000),
            user_message_string("after", 1700000010),
        ],
    );
    let outcome = fx.discover(fx.roots_default());
    assert_eq!(outcome.parsed.len(), 1);
    assert_eq!(outcome.parsed[0].id, "late-hdr");
}

// ===========================================================================
// TITLE RESOLUTION: header / title / title_change
// ===========================================================================

#[test]
fn title_sidecar_provides_initial_title() {
    let fx = Fixture::new();
    fx.write(
        &fx.default_agent_root,
        &fx.encoded_ws(),
        "t.jsonl",
        &[
            title_sidecar("Sidecar Title"),
            header_v3("t", &fx.workspace, 1700000000),
            user_message_string("msg", 1700000010),
        ],
    );
    let outcome = fx.discover(fx.roots_default());
    assert_eq!(outcome.parsed[0].title.as_deref(), Some("Sidecar Title"));
}

#[test]
fn header_title_metadata_wins_over_earlier_sidecar() {
    let fx = Fixture::new();
    fx.write(
        &fx.default_agent_root,
        &fx.encoded_ws(),
        "h.jsonl",
        &[
            title_sidecar("Old"),
            header_v3_titled("h", &fx.workspace, 1700000000, "Header Title"),
        ],
    );
    let outcome = fx.discover(fx.roots_default());
    assert_eq!(outcome.parsed[0].title.as_deref(), Some("Header Title"));
}

#[test]
fn title_change_overrides_header_and_sidecar() {
    let fx = Fixture::new();
    fx.write(
        &fx.default_agent_root,
        &fx.encoded_ws(),
        "tc.jsonl",
        &[
            title_sidecar("Side"),
            header_v3_titled("tc", &fx.workspace, 1700000000, "Header"),
            title_change("Changed"),
        ],
    );
    let outcome = fx.discover(fx.roots_default());
    assert_eq!(outcome.parsed[0].title.as_deref(), Some("Changed"));
}

#[test]
fn latest_title_change_wins() {
    let fx = Fixture::new();
    fx.write(
        &fx.default_agent_root,
        &fx.encoded_ws(),
        "multi.jsonl",
        &[
            header_v3("multi", &fx.workspace, 1700000000),
            title_change("First"),
            title_change("Second"),
            title_change("Third"),
        ],
    );
    let outcome = fx.discover(fx.roots_default());
    assert_eq!(outcome.parsed[0].title.as_deref(), Some("Third"));
}

#[test]
fn title_falls_back_to_summary_from_first_human_input() {
    let fx = Fixture::new();
    fx.write(
        &fx.default_agent_root,
        &fx.encoded_ws(),
        "fallback.jsonl",
        &[
            header_v3("fb", &fx.workspace, 1700000000),
            user_message_string("Fix the parser bug now", 1700000010),
            user_message_string("second", 1700000020),
        ],
    );
    let outcome = fx.discover(fx.roots_default());
    let title = outcome.parsed[0].title.clone().unwrap();
    assert!(title.starts_with("Fix the parser bug now"));
}

#[test]
fn title_none_when_no_title_and_no_user_messages() {
    let fx = Fixture::new();
    fx.write(
        &fx.default_agent_root,
        &fx.encoded_ws(),
        "empty.jsonl",
        &[header_v3("empty", &fx.workspace, 1700000000)],
    );
    let outcome = fx.discover(fx.roots_default());
    assert!(outcome.parsed[0].title.is_none());
}

// ===========================================================================
// USER MESSAGE EXTRACTION + attribution filtering
// ===========================================================================

#[test]
fn extracts_string_user_message() {
    let fx = Fixture::new();
    fx.write(
        &fx.default_agent_root,
        &fx.encoded_ws(),
        "s.jsonl",
        &[
            header_v3("s", &fx.workspace, 1700000000),
            user_message_string("hello", 1700000010),
        ],
    );
    let outcome = fx.discover(fx.roots_default());
    assert_eq!(outcome.parsed[0].messages.len(), 1);
    assert_eq!(outcome.parsed[0].messages[0].text, "hello");
}

#[test]
fn extracts_text_plus_image_with_placeholder_not_base64() {
    let fx = Fixture::new();
    fx.write(
        &fx.default_agent_root,
        &fx.encoded_ws(),
        "img.jsonl",
        &[
            header_v3("img", &fx.workspace, 1700000000),
            user_message_blocks("look here", "image/png", 1700000010),
        ],
    );
    let outcome = fx.discover(fx.roots_default());
    let msg = &outcome.parsed[0].messages[0];
    assert_eq!(msg.text, "look here");
    assert_eq!(msg.attachments.len(), 1);
    let display = msg.attachments[0].to_display();
    assert!(display.contains("[image]"));
    assert!(display.contains("image/png"));
    assert!(!display.contains("iVBOR"));
}

#[test]
fn extracts_image_only_message() {
    let fx = Fixture::new();
    fx.write(
        &fx.default_agent_root,
        &fx.encoded_ws(),
        "imgonly.jsonl",
        &[
            header_v3("io", &fx.workspace, 1700000000),
            user_message_image_only("image/jpeg", 1700000010),
        ],
    );
    let outcome = fx.discover(fx.roots_default());
    let msg = &outcome.parsed[0].messages[0];
    assert!(msg.text.is_empty());
    assert_eq!(msg.attachments.len(), 1);
}

#[test]
fn excludes_assistant_messages() {
    let fx = Fixture::new();
    fx.write(
        &fx.default_agent_root,
        &fx.encoded_ws(),
        "a.jsonl",
        &[
            header_v3("a", &fx.workspace, 1700000000),
            user_message_string("user q", 1700000010),
            assistant_message("agent a"),
        ],
    );
    let outcome = fx.discover(fx.roots_default());
    assert_eq!(outcome.parsed[0].messages.len(), 1);
    assert_eq!(outcome.parsed[0].messages[0].text, "user q");
}

#[test]
fn excludes_injected_user_messages_by_attribution() {
    let fx = Fixture::new();
    fx.write(
        &fx.default_agent_root,
        &fx.encoded_ws(),
        "inj.jsonl",
        &[
            header_v3("inj", &fx.workspace, 1700000000),
            injected_user_message("agent-injected text", 1700000010),
            user_message_string("real user text", 1700000020),
        ],
    );
    let outcome = fx.discover(fx.roots_default());
    let msgs = &outcome.parsed[0].messages;
    assert_eq!(msgs.len(), 1, "injected user-role message must be excluded");
    assert_eq!(msgs[0].text, "real user text");
}

#[test]
fn excludes_nested_metadata_injected_user_messages() {
    let fx = Fixture::new();
    fx.write(
        &fx.default_agent_root,
        &fx.encoded_ws(),
        "nested-inj.jsonl",
        &[
            header_v3("nested-inj", &fx.workspace, 1700000000),
            json!({
                "type": "user",
                "message": {
                    "role": "user",
                    "meta": { "automated": true },
                    "content": "automated user-role message",
                },
            }),
            user_message_string("real user text", 1700000020),
        ],
    );

    let outcome = fx.discover(fx.roots_default());
    let messages = &outcome.parsed[0].messages;
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].text, "real user text");
}

#[test]
fn injection_wrappers_collapsed_in_user_messages() {
    let fx = Fixture::new();
    fx.write(
        &fx.default_agent_root,
        &fx.encoded_ws(),
        "wrap.jsonl",
        &[
            header_v3("wrap", &fx.workspace, 1700000000),
            user_message_string("<skill>hidden</skill> visible", 1700000010),
        ],
    );
    let outcome = fx.discover(fx.roots_default());
    assert_eq!(outcome.parsed[0].messages[0].text, "hidden visible");
}

// ===========================================================================
// FOREIGN SESSION IMPORT → safe badge, new OMP ID retained
// ===========================================================================

#[test]
fn import_creates_safe_badge_and_keeps_new_omp_id() {
    let fx = Fixture::new();
    let origin_cwd = fx.home().join("origin-repo");
    fs::create_dir_all(&origin_cwd).unwrap();
    fx.write(
        &fx.default_agent_root,
        &fx.encoded_ws(),
        "imp.jsonl",
        &[
            title_sidecar("Imported Session"),
            header_v3("omp-new-id", &fx.workspace, 1700000000),
            foreign_import("codex", "codex-origin-id-1234", &origin_cwd),
        ],
    );
    let outcome = fx.discover(fx.roots_default());
    assert_eq!(outcome.parsed.len(), 1);
    let parsed = &outcome.parsed[0];
    // Resumable identity is the NEW OMP header id, never the origin id.
    assert_eq!(parsed.id, "omp-new-id");
    let badge = parsed.import.as_ref().expect("import badge present");
    assert_eq!(badge.source_kind, "codex");
    assert_eq!(badge.origin_id.as_deref(), Some("codex-origin-id-1234"));
    assert_eq!(badge.origin_cwd.as_deref(), Some(origin_cwd.as_path()));
    // The badge display must not expose the full origin id as resumable.
    let display = badge.to_display();
    assert!(display.contains("imported from codex"));
    assert!(display.contains("origin:codex-or"));
    assert!(!display.contains("codex-origin-id-1234"));

    let session = parsed.clone().into_session(
        &fx.roots_default(),
        crate::session::RiskStatus::Normal,
        crate::session::ActivityStatus::Unknown,
    );
    let title = session.title.expect("safe import badge is user-visible");
    assert!(title.contains("Imported Session"));
    assert!(title.contains("imported from codex origin:codex-or"));
    assert!(!title.contains("codex-origin-id-1234"));
    assert!(!title.contains(origin_cwd.to_str().unwrap()));
}

#[test]
fn import_never_merges_with_origin_session_identity() {
    // The native_locator is the OMP transcript path; the key never references
    // the origin locator. Even with the same origin_id across two OMP files,
    // they remain distinct OMP sessions.
    let fx = Fixture::new();
    let origin_cwd = fx.home().join("origin");
    fs::create_dir_all(&origin_cwd).unwrap();
    let p1 = fx.write(
        &fx.default_agent_root,
        &fx.encoded_ws(),
        "a.jsonl",
        &[
            header_v3("omp-1", &fx.workspace, 1700000000),
            foreign_import("claude", "shared-origin-id", &origin_cwd),
        ],
    );
    let p2 = fx.write(
        &fx.default_agent_root,
        &fx.encoded_ws(),
        "b.jsonl",
        &[
            header_v3("omp-2", &fx.workspace, 1700000000),
            foreign_import("claude", "shared-origin-id", &origin_cwd),
        ],
    );
    let outcome = fx.discover(fx.roots_default());
    assert_eq!(outcome.parsed.len(), 2);
    assert_ne!(outcome.parsed[0].id, outcome.parsed[1].id);
    assert_ne!(
        outcome.parsed[0].transcript_path,
        outcome.parsed[1].transcript_path
    );
    let _ = (p1, p2);
}

// ===========================================================================
// TIMESTAMP FALLBACK CHAIN
// ===========================================================================

#[test]
fn activity_time_prefers_message_then_header_then_mtime() {
    let fx = Fixture::new();
    let path = fx.write(
        &fx.default_agent_root,
        &fx.encoded_ws(),
        "ts.jsonl",
        &[
            header_v3("ts", &fx.workspace, 1700000000),
            user_message_string("hi", 1700000050),
        ],
    );
    let outcome = fx.discover(fx.roots_default());
    let parsed = &outcome.parsed[0];
    assert_eq!(
        parsed.activity_time,
        Some(SystemTime::UNIX_EPOCH + Duration::from_secs(1700000050))
    );

    // Header-only → header time.
    let bounds = crate::preview::jsonl::Bounds::default();
    let path2 = fx.write(
        &fx.default_agent_root,
        &fx.encoded_ws(),
        "ho.jsonl",
        &[header_v3("ho", &fx.workspace, 1700000000)],
    );
    let result2 = crate::preview::jsonl::read_file(&path2, &bounds).unwrap();
    let parsed2 = omp::extract_session_pub(&path2, &result2, None).unwrap();
    assert_eq!(
        parsed2.activity_time,
        Some(SystemTime::UNIX_EPOCH + Duration::from_secs(1700000000))
    );

    // No header timestamp → mtime.
    let header_no_ts = json!({ "type": "session", "id": "nts", "cwd": fx.workspace });
    let path3 = fx.write(
        &fx.default_agent_root,
        &fx.encoded_ws(),
        "nts.jsonl",
        &[header_no_ts],
    );
    let mtime = fs::metadata(&path3).unwrap().modified().unwrap();
    let result3 = crate::preview::jsonl::read_file(&path3, &bounds).unwrap();
    let parsed3 = omp::extract_session_pub(&path3, &result3, Some(mtime)).unwrap();
    assert_eq!(parsed3.activity_time, Some(mtime));
    let _ = path;
}

// ===========================================================================
// SESSION CONSTRUCTION + RISK
// ===========================================================================

#[test]
fn into_session_builds_supported_session_with_profile_identity() {
    let fx = Fixture::new();
    let named_root = fx.profile_agent_root("work");
    fx.write_flat(
        &named_root,
        "s.jsonl",
        &[
            title_sidecar("Work Session"),
            header_v3("s", &fx.workspace, 1700000000),
        ],
    );
    let outcome = fx.discover(fx.roots_named("work"));
    let session = outcome.parsed[0].clone().into_session(
        &fx.roots_named("work"),
        crate::session::RiskStatus::Normal,
        crate::session::ActivityStatus::Unknown,
    );
    assert_eq!(session.key.agent, OsString::from("omp"));
    assert_eq!(
        session.key.profile.as_deref(),
        Some(std::ffi::OsStr::new("work"))
    );
    assert_eq!(session.resumable_id, OsString::from("s"));
    assert_eq!(session.title.as_deref(), Some("Work Session"));
    assert_eq!(session.support, crate::session::SupportStatus::Supported);
}

#[test]
fn broad_workspace_risk_flagged_for_home_and_root() {
    let parsed = ParsedSession {
        id: "r".into(),
        workspace: Some(PathBuf::from("/")),
        header_time: None,
        title: None,
        messages: vec![],
        transcript_path: PathBuf::from("/x.jsonl"),
        file_mtime: None,
        activity_time: None,
        import: None,
    };
    assert_eq!(
        omp::risk_status(&parsed, Some(Path::new("/"))),
        crate::session::RiskStatus::BroadWorkspace
    );
}

// ===========================================================================
// IMPORT BADGE UNIT TESTS
// ===========================================================================

#[test]
fn import_badge_display_truncates_origin_id() {
    let badge = ImportBadge {
        source_kind: "codex".into(),
        origin_id: Some("abcdef1234567890".into()),
        origin_cwd: None,
    };
    let display = badge.to_display();
    assert!(display.contains("imported from codex"));
    assert!(display.contains("origin:abcdef1"));
    assert!(!display.contains("abcdef1234567890"));
}

#[test]
fn import_badge_without_origin_id() {
    let badge = ImportBadge {
        source_kind: "claude".into(),
        origin_id: None,
        origin_cwd: None,
    };
    let display = badge.to_display();
    assert_eq!(display, "imported from claude");
}

#[test]
fn import_badge_rejects_untrusted_metadata() {
    let badge = ImportBadge {
        source_kind: "/private/source\nnot-a-kind".into(),
        origin_id: Some("git@github.com:private/repo".into()),
        origin_cwd: None,
    };
    assert_eq!(badge.to_display(), "imported from unknown");
}

#[test]
fn parse_import_pub_handles_alternate_keys() {
    let v = json!({
        "kind": "codex",
        "source_id": "sid",
        "source_cwd": "/path",
    });
    let badge = omp::parse_import_pub(&v).unwrap();
    assert_eq!(badge.source_kind, "codex");
    assert_eq!(badge.origin_id.as_deref(), Some("sid"));
    assert_eq!(
        badge.origin_cwd.as_deref(),
        Some(std::path::Path::new("/path"))
    );
}
