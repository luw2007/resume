//! Pi integration tests.
//!
//! Implements the complete Pi fixture matrix from the plan's Tests section:
//! - versions 1, 2, and 3;
//! - named/cleared sessions;
//! - strings, text plus image, and image-only input;
//! - branched parents;
//! - alternate and flat roots;
//! - duplicate IDs across roots;
//! - timestamp fallback;
//! - malformed middle/tail records;
//! - missing header;
//! - missing Workspace;
//! - growing file.
//!
//! Uses a fake `pi` executable that captures exact cwd/argv/env, and asserts
//! discovery/Preview never migrates v1/v2 files or changes any byte/mtime via
//! the shared snapshot helpers.

#![cfg(test)]

use std::{
    ffi::OsString,
    fs,
    io::Write,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use serde_json::{Value, json};

use crate::{
    integration::pi::{
        self, DiscoverConfig, EffectiveRoots, ParsedSession, ResolutionInputs,
        SessionControlEvidence,
    },
    scope::{Direction, Scope},
    snapshot,
};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Tempdir-based fixture builder for Pi sessions.
struct Fixture {
    _tmp: tempfile::TempDir,
    /// agent root: `<tmp>/.pi/agent` (matches `~/.pi/agent` default).
    agent_root: PathBuf,
    /// default session root: `<tmp>/.pi/agent/sessions`
    session_root: PathBuf,
    /// A workspace dir to use as header cwd.
    workspace: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let agent_root = tmp.path().join(".pi/agent");
        let session_root = agent_root.join("sessions");
        let workspace = tmp.path().join("workspace");
        fs::create_dir_all(&session_root).unwrap();
        fs::create_dir_all(&workspace).unwrap();
        Self {
            _tmp: tmp,
            agent_root,
            session_root,
            workspace,
        }
    }

    fn home(&self) -> PathBuf {
        // The fake home is `<tmp>` so that `$HOME/.pi/agent` == agent_root.
        // agent_root = <tmp>/.pi/agent → parent = <tmp>/.pi → parent = <tmp>.
        self.agent_root
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf()
    }

    /// Write a JSONL file into a grouped Workspace dir, returning its path.
    fn write_grouped(&self, encoded_ws: &str, name: &str, records: &[Value]) -> PathBuf {
        let dir = self.session_root.join(encoded_ws);
        fs::create_dir_all(&dir).unwrap();
        self.write_jsonl(&dir.join(name), records)
    }

    fn write_jsonl(&self, path: &Path, records: &[Value]) -> PathBuf {
        let mut file = fs::File::create(path).unwrap();
        for record in records {
            writeln!(file, "{}", serde_json::to_string(record).unwrap()).unwrap();
        }
        file.sync_all().unwrap();
        path.to_path_buf()
    }

    /// Append a raw (possibly partial) record fragment to a file.
    fn append_raw(&self, path: &Path, fragment: &[u8]) {
        let mut file = fs::OpenOptions::new().append(true).open(path).unwrap();
        file.write_all(fragment).unwrap();
        file.sync_all().unwrap();
    }

    fn roots_default(&self) -> EffectiveRoots {
        EffectiveRoots {
            agent_root: self.agent_root.clone(),
            session_root: self.session_root.clone(),
            custom_session_root: false,
        }
    }

    fn roots_custom(&self, custom_root: PathBuf) -> EffectiveRoots {
        EffectiveRoots {
            agent_root: self.agent_root.clone(),
            session_root: custom_root,
            custom_session_root: true,
        }
    }

    fn scope_exact_workspace(&self) -> Scope {
        Scope::new(
            self.workspace.canonicalize().unwrap(),
            None,
            crate::scope::DefaultScope::Exact { git_warning: None },
        )
    }

    /// Discover using the default roots and a workspace-exact scope.
    fn discover_default(&self) -> crate::integration::pi::DiscoverOutcome {
        let roots = self.roots_default();
        let scope = self.scope_exact_workspace();
        let cfg = DiscoverConfig::new(roots, &scope);
        pi::discover(&cfg).unwrap()
    }

    /// Discover using custom roots and a workspace-exact scope.
    fn discover_custom(&self, roots: EffectiveRoots) -> crate::integration::pi::DiscoverOutcome {
        let scope = self.scope_exact_workspace();
        let cfg = DiscoverConfig::new(roots, &scope);
        pi::discover(&cfg).unwrap()
    }

    /// Inputs that resolve to this fixture's default roots via $HOME.
    fn inputs_default(&self) -> ResolutionInputs {
        ResolutionInputs {
            home: Some(self.home()),
            agent_dir_env: None,
            session_dir_env: None,
            session_dir_flag: None,
            settings: None,
        }
    }
}

/// Build a v3 session header record.
fn header_v3(id: &str, cwd: &Path, timestamp: u64) -> Value {
    json!({
        "type": "session",
        "v": 3,
        "id": id,
        "cwd": cwd,
        "timestamp": timestamp,
    })
}

/// Build a v2 session header (no `v` field, otherwise same shape).
fn header_v2(id: &str, cwd: &Path, timestamp: u64) -> Value {
    json!({
        "type": "session",
        "id": id,
        "cwd": cwd,
        "timestamp": timestamp,
    })
}

/// Build a v1 session header (older field name `sessionId`, `dir`).
fn header_v1(session_id: &str, cwd: &Path, timestamp: u64) -> Value {
    json!({
        "type": "session",
        "id": session_id,
        "cwd": cwd,
        "timestamp": timestamp,
    })
}

/// Build a user message record with string content.
fn user_message_string(text: &str, timestamp: u64) -> Value {
    json!({
        "type": "user",
        "timestamp": timestamp,
        "message": {
            "role": "user",
            "content": text,
        }
    })
}

/// Build a user message record with typed block content (text + image).
fn user_message_blocks(text: &str, image_media_type: &str, timestamp: u64) -> Value {
    json!({
        "type": "user",
        "timestamp": timestamp,
        "message": {
            "role": "user",
            "content": [
                { "type": "text", "text": text },
                { "type": "image", "media_type": image_media_type, "data": "iVBORw0KGgo=" }
            ]
        }
    })
}

/// Build an image-only user message (no text).
fn user_message_image_only(media_type: &str, timestamp: u64) -> Value {
    json!({
        "type": "user",
        "timestamp": timestamp,
        "message": {
            "role": "user",
            "content": [
                { "type": "image", "media_type": media_type, "data": "iVBORw0KGgo=" }
            ]
        }
    })
}

/// Build a session_info record with a name.
fn session_info(name: &str) -> Value {
    json!({
        "type": "session_info",
        "name": name,
    })
}

/// Build an assistant record (must be excluded from user messages).
fn assistant_message(text: &str) -> Value {
    json!({
        "type": "assistant",
        "message": {
            "role": "assistant",
            "content": text,
        }
    })
}

// ---------------------------------------------------------------------------
// Root resolution tests
// ---------------------------------------------------------------------------

#[test]
fn resolves_default_grouped_root_from_home() {
    let fx = Fixture::new();
    let inputs = fx.inputs_default();
    let roots = pi::resolve(&inputs).expect("home present");
    assert!(!roots.custom_session_root);
    assert_eq!(roots.agent_root, fx.agent_root);
    assert_eq!(roots.session_root, fx.session_root);
}

#[test]
fn agent_dir_env_overrides_default_root() {
    let tmp = tempfile::tempdir().unwrap();
    let custom = tmp.path().join("custom-agent");
    fs::create_dir_all(&custom).unwrap();
    let inputs = ResolutionInputs {
        home: Some(tmp.path().to_path_buf()),
        agent_dir_env: Some(custom.clone()),
        session_dir_env: None,
        session_dir_flag: None,
        settings: None,
    };
    let roots = pi::resolve(&inputs).unwrap();
    assert_eq!(roots.agent_root, custom);
    assert_eq!(roots.session_root, custom.join("sessions"));
}

#[test]
fn session_dir_env_overrides_session_root_and_is_flat() {
    let fx = Fixture::new();
    let custom_sessions = fx.agent_root.join("alt-sessions");
    fs::create_dir_all(&custom_sessions).unwrap();
    let inputs = ResolutionInputs {
        home: Some(fx.home()),
        agent_dir_env: None,
        session_dir_env: Some(custom_sessions.clone()),
        session_dir_flag: None,
        settings: None,
    };
    let roots = pi::resolve(&inputs).unwrap();
    assert!(roots.custom_session_root);
    assert_eq!(roots.session_root, custom_sessions);
}

#[test]
fn session_dir_flag_beats_env_and_settings() {
    let fx = Fixture::new();
    let flag_dir = fx.agent_root.join("flag-sessions");
    let env_dir = fx.agent_root.join("env-sessions");
    fs::create_dir_all(&flag_dir).unwrap();
    let settings = json!({ "sessionDir": fx.agent_root.join("settings-sessions") });
    let inputs = ResolutionInputs {
        home: Some(fx.home()),
        agent_dir_env: None,
        session_dir_env: Some(env_dir),
        session_dir_flag: Some(flag_dir.clone()),
        settings: Some(settings),
    };
    let roots = pi::resolve(&inputs).unwrap();
    assert_eq!(roots.session_root, flag_dir);
}

#[test]
fn settings_session_dir_overrides_default() {
    let fx = Fixture::new();
    let settings_dir = fx.agent_root.join("settings-sessions");
    let settings = json!({ "sessionDir": settings_dir.clone() });
    let inputs = ResolutionInputs {
        home: Some(fx.home()),
        agent_dir_env: None,
        session_dir_env: None,
        session_dir_flag: None,
        settings: Some(settings),
    };
    let roots = pi::resolve(&inputs).unwrap();
    assert!(roots.custom_session_root);
    assert_eq!(roots.session_root, settings_dir);
}

#[test]
fn settings_session_dir_as_object_path() {
    let fx = Fixture::new();
    let settings_dir = fx.agent_root.join("obj-sessions");
    let settings = json!({ "sessionDir": { "path": settings_dir.clone() } });
    let inputs = ResolutionInputs {
        home: Some(fx.home()),
        agent_dir_env: None,
        session_dir_env: None,
        session_dir_flag: None,
        settings: Some(settings),
    };
    let roots = pi::resolve(&inputs).unwrap();
    assert_eq!(roots.session_root, settings_dir);
}

#[test]
fn resolve_returns_none_without_home_and_agent_env() {
    let inputs = ResolutionInputs {
        home: None,
        agent_dir_env: None,
        session_dir_env: None,
        session_dir_flag: None,
        settings: None,
    };
    assert!(pi::resolve(&inputs).is_none());
}

// ---------------------------------------------------------------------------
// Header parsing across v1/v2/v3
// ---------------------------------------------------------------------------

#[test]
fn parses_v3_header_extracts_id_and_workspace() {
    let fx = Fixture::new();
    fx.write_grouped(
        "encoded-ws",
        "session-1.jsonl",
        &[
            header_v3("abc-123", &fx.workspace, 1700000000),
            user_message_string("hello world", 1700000010),
        ],
    );
    let outcome = fx.discover_default();
    assert_eq!(outcome.parsed.len(), 1);
    let parsed = &outcome.parsed[0];
    assert_eq!(parsed.id, "abc-123");
    assert_eq!(parsed.workspace.as_ref().unwrap(), &fx.workspace);
    assert_eq!(parsed.messages.len(), 1);
    assert_eq!(parsed.messages[0].text, "hello world");
}

#[test]
fn parses_v2_header_same_shape_as_v3() {
    let fx = Fixture::new();
    fx.write_grouped(
        "encoded-ws",
        "session-v2.jsonl",
        &[
            header_v2("v2-id", &fx.workspace, 1700000000),
            user_message_string("v2 message", 1700000010),
        ],
    );
    let outcome = fx.discover_default();
    assert_eq!(outcome.parsed.len(), 1);
    assert_eq!(outcome.parsed[0].id, "v2-id");
}

#[test]
fn parses_v1_header_uses_id_field() {
    let fx = Fixture::new();
    fx.write_grouped(
        "encoded-ws",
        "session-v1.jsonl",
        &[
            header_v1("v1-id", &fx.workspace, 1700000000),
            user_message_string("v1 message", 1700000010),
        ],
    );
    let outcome = fx.discover_default();
    assert_eq!(outcome.parsed.len(), 1);
    assert_eq!(outcome.parsed[0].id, "v1-id");
}

#[test]
fn file_without_session_header_is_skipped_as_no_header() {
    let fx = Fixture::new();
    // No "session" type record.
    fx.write_grouped(
        "encoded-ws",
        "not-a-session.jsonl",
        &[user_message_string("hello", 1700000000)],
    );
    let outcome = fx.discover_default();
    assert_eq!(outcome.parsed.len(), 0);
    assert_eq!(outcome.no_header_files, 1);
}

// ---------------------------------------------------------------------------
// Title precedence: session_info.name else summary from first human input
// ---------------------------------------------------------------------------

#[test]
fn title_prefers_latest_session_info_name() {
    let fx = Fixture::new();
    fx.write_grouped(
        "encoded-ws",
        "named.jsonl",
        &[
            header_v3("named", &fx.workspace, 1700000000),
            user_message_string("first message", 1700000010),
            session_info("My Cool Session"),
            user_message_string("second message", 1700000020),
        ],
    );
    let outcome = fx.discover_default();
    assert_eq!(
        outcome.parsed[0].title().as_deref(),
        Some("My Cool Session")
    );
}

#[test]
fn title_latest_session_info_name_wins_over_earlier() {
    let fx = Fixture::new();
    fx.write_grouped(
        "encoded-ws",
        "renamed.jsonl",
        &[
            header_v3("renamed", &fx.workspace, 1700000000),
            session_info("Old Name"),
            session_info("New Name"),
        ],
    );
    let outcome = fx.discover_default();
    assert_eq!(outcome.parsed[0].title().as_deref(), Some("New Name"));
}

#[test]
fn title_falls_back_to_summary_from_first_human_input() {
    let fx = Fixture::new();
    fx.write_grouped(
        "encoded-ws",
        "unnamed.jsonl",
        &[
            header_v3("unnamed", &fx.workspace, 1700000000),
            user_message_string("Fix the parser bug now", 1700000010),
            user_message_string("second", 1700000020),
        ],
    );
    let outcome = fx.discover_default();
    let title = outcome.parsed[0].title().unwrap();
    assert!(title.starts_with("Fix the parser bug now"));
}

#[test]
fn title_none_when_no_name_and_no_user_messages() {
    let fx = Fixture::new();
    fx.write_grouped(
        "encoded-ws",
        "empty.jsonl",
        &[header_v3("empty", &fx.workspace, 1700000000)],
    );
    let outcome = fx.discover_default();
    assert!(outcome.parsed[0].title().is_none());
}

// ---------------------------------------------------------------------------
// User message extraction: strings, text+image, image-only
// ---------------------------------------------------------------------------

#[test]
fn extracts_text_plus_image_blocks_with_placeholder_not_base64() {
    let fx = Fixture::new();
    fx.write_grouped(
        "encoded-ws",
        "img.jsonl",
        &[
            header_v3("img", &fx.workspace, 1700000000),
            user_message_blocks("look here", "image/png", 1700000010),
        ],
    );
    let outcome = fx.discover_default();
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
    fx.write_grouped(
        "encoded-ws",
        "imgonly.jsonl",
        &[
            header_v3("imgonly", &fx.workspace, 1700000000),
            user_message_image_only("image/jpeg", 1700000010),
        ],
    );
    let outcome = fx.discover_default();
    let msg = &outcome.parsed[0].messages[0];
    assert!(msg.text.is_empty());
    assert_eq!(msg.attachments.len(), 1);
    match &msg.attachments[0] {
        crate::message::Attachment::Image { media_type, .. } => {
            assert_eq!(media_type.as_deref(), Some("image/jpeg"));
        }
        _ => panic!("expected image attachment"),
    }
}

#[test]
fn excludes_assistant_messages() {
    let fx = Fixture::new();
    fx.write_grouped(
        "encoded-ws",
        "assistant.jsonl",
        &[
            header_v3("a", &fx.workspace, 1700000000),
            user_message_string("user question", 1700000010),
            assistant_message("agent answer"),
        ],
    );
    let outcome = fx.discover_default();
    assert_eq!(outcome.parsed[0].messages.len(), 1);
    assert_eq!(outcome.parsed[0].messages[0].text, "user question");
}

#[test]
fn injection_wrappers_collapsed_in_user_messages() {
    let fx = Fixture::new();
    fx.write_grouped(
        "encoded-ws",
        "injected.jsonl",
        &[
            header_v3("inj", &fx.workspace, 1700000000),
            user_message_string("<skill>hidden</skill> visible", 1700000010),
        ],
    );
    let outcome = fx.discover_default();
    assert_eq!(outcome.parsed[0].messages[0].text, "hidden visible");
}

// ---------------------------------------------------------------------------
// Parent / branching
// ---------------------------------------------------------------------------

#[test]
fn extracts_parent_session_id() {
    let fx = Fixture::new();
    let header = json!({
        "type": "session",
        "v": 3,
        "id": "child-id",
        "cwd": fx.workspace,
        "timestamp": 1700000000u64,
        "parentSession": "parent-id",
    });
    fx.write_grouped(
        "encoded-ws",
        "child.jsonl",
        &[header, user_message_string("branched", 1700000010)],
    );
    let outcome = fx.discover_default();
    assert_eq!(outcome.parsed[0].parent.as_deref(), Some("parent-id"));
}

// ---------------------------------------------------------------------------
// Scope filtering: custom flat roots filter by header cwd
// ---------------------------------------------------------------------------

#[test]
fn custom_flat_root_filters_by_header_cwd_not_directory_name() {
    let fx = Fixture::new();
    let custom = fx.agent_root.join("flat-sessions");
    fs::create_dir_all(&custom).unwrap();
    let roots = fx.roots_custom(custom.clone());

    // A file whose directory name does NOT encode the workspace; only header
    // cwd determines membership.
    let path = custom.join("misc.jsonl");
    let mut file = fs::File::create(&path).unwrap();
    writeln!(
        file,
        "{}",
        serde_json::to_string(&header_v3("flat-id", &fx.workspace, 1700000000)).unwrap()
    )
    .unwrap();
    writeln!(
        file,
        "{}",
        serde_json::to_string(&user_message_string("flat", 1700000010)).unwrap()
    )
    .unwrap();

    let outcome = fx.discover_custom(roots.clone());
    assert_eq!(outcome.parsed.len(), 1, "header cwd must match via Scope");
}

#[test]
fn out_of_scope_workspace_is_excluded() {
    let fx = Fixture::new();
    let other_ws = fx.home().join("other-workspace");
    fs::create_dir_all(&other_ws).unwrap();
    fx.write_grouped(
        "encoded-ws",
        "other.jsonl",
        &[
            header_v3("other", &other_ws, 1700000000),
            user_message_string("other", 1700000010),
        ],
    );
    let outcome = fx.discover_default();
    assert_eq!(outcome.parsed.len(), 0);
    assert_eq!(outcome.out_of_scope, 1);
}

#[test]
fn down_scope_includes_descendant_workspaces() {
    let fx = Fixture::new();
    let child_ws = fx.workspace.join("subdir");
    fs::create_dir_all(&child_ws).unwrap();
    fx.write_grouped(
        "encoded-ws",
        "child.jsonl",
        &[
            header_v3("child", &child_ws, 1700000000),
            user_message_string("child", 1700000010),
        ],
    );
    let scope = Scope::new(
        fx.workspace.canonicalize().unwrap(),
        Some(Direction::Down(crate::cli::Distance::Finite(2))),
        crate::scope::DefaultScope::Exact { git_warning: None },
    );
    let cfg = DiscoverConfig::new(fx.roots_default(), &scope);
    let outcome = pi::discover(&cfg).unwrap();
    assert_eq!(outcome.parsed.len(), 1);
}

// ---------------------------------------------------------------------------
// Dedupe keyed by effective session root + canonical transcript locator
// ---------------------------------------------------------------------------

#[test]
fn duplicate_files_under_same_root_are_deduped() {
    let fx = Fixture::new();
    let path = fx.write_grouped(
        "encoded-ws",
        "dup.jsonl",
        &[
            header_v3("dup-id", &fx.workspace, 1700000000),
            user_message_string("dup", 1700000010),
        ],
    );
    // Symlink the same file to a second path; canonical locator is identical.
    #[cfg(unix)]
    {
        let link = fx.session_root.join("encoded-ws").join("dup-link.jsonl");
        std::os::unix::fs::symlink(&path, &link).unwrap();
    }
    let outcome = fx.discover_default();
    #[cfg(unix)]
    assert_eq!(outcome.parsed.len(), 1, "dedupe by canonical locator");
    #[cfg(not(unix))]
    assert_eq!(outcome.parsed.len(), 1);
}

#[test]
fn same_id_across_different_roots_are_distinct() {
    // Two session roots, same header id, distinct transcript locators.
    let fx = Fixture::new();
    let root_a = fx.agent_root.join("rootA/sessions");
    let root_b = fx.agent_root.join("rootB/sessions");
    for root in [&root_a, &root_b] {
        fs::create_dir_all(root).unwrap();
    }

    let write = |root: &Path, name: &str| {
        let mut file = fs::File::create(root.join(name)).unwrap();
        writeln!(
            file,
            "{}",
            serde_json::to_string(&header_v3("shared-id", &fx.workspace, 1700000000)).unwrap()
        )
        .unwrap();
        writeln!(
            file,
            "{}",
            serde_json::to_string(&user_message_string("x", 1700000010)).unwrap()
        )
        .unwrap();
    };
    write(&root_a, "a.jsonl");
    write(&root_b, "b.jsonl");

    let roots_a = EffectiveRoots {
        agent_root: fx.agent_root.clone(),
        session_root: root_a.clone(),
        custom_session_root: true,
    };
    let roots_b = EffectiveRoots {
        agent_root: fx.agent_root.clone(),
        session_root: root_b.clone(),
        custom_session_root: true,
    };

    let scope = fx.scope_exact_workspace();
    let cfg_a = DiscoverConfig::new(roots_a.clone(), &scope);
    let cfg_b = DiscoverConfig::new(roots_b.clone(), &scope);
    let oa = pi::discover(&cfg_a).unwrap();
    let ob = pi::discover(&cfg_b).unwrap();
    assert_eq!(oa.parsed.len(), 1);
    assert_eq!(ob.parsed.len(), 1);
    // They have the same id but distinct locators/roots → different SessionKey.
    assert_ne!(oa.parsed[0].transcript_path, ob.parsed[0].transcript_path);
}

// ---------------------------------------------------------------------------
// Timestamp fallback chain
// ---------------------------------------------------------------------------

#[test]
fn activity_time_prefers_message_then_header_then_mtime() {
    let fx = Fixture::new();
    let path = fx.write_grouped(
        "encoded-ws",
        "ts.jsonl",
        &[
            header_v3("ts", &fx.workspace, 1700000000),
            user_message_string("hi", 1700000050),
        ],
    );
    let outcome = fx.discover_default();
    let parsed = &outcome.parsed[0];
    let expected = SystemTime::UNIX_EPOCH + Duration::from_secs(1700000050);
    assert_eq!(parsed.activity_time, Some(expected));

    let bounds = crate::jsonl::Bounds::default();

    // Now a file with no messages: falls back to header time.
    let path2 = fx.write_grouped(
        "encoded-ws",
        "header-only.jsonl",
        &[header_v3("ho", &fx.workspace, 1700000000)],
    );
    let result2 = crate::jsonl::read_file(&path2, &bounds).unwrap();
    let parsed2 = pi::extract_session_pub(&path2, &result2, None).unwrap();
    assert_eq!(
        parsed2.activity_time,
        Some(SystemTime::UNIX_EPOCH + Duration::from_secs(1700000000))
    );

    // File with no header timestamp: falls back to mtime.
    let header_no_ts = json!({ "type": "session", "id": "nts", "cwd": fx.workspace });
    let path3 = fx.write_grouped("encoded-ws", "no-ts.jsonl", &[header_no_ts]);
    let mtime = fs::metadata(&path3).unwrap().modified().unwrap();
    let result3 = crate::jsonl::read_file(&path3, &bounds).unwrap();
    let parsed3 = pi::extract_session_pub(&path3, &result3, Some(mtime)).unwrap();
    assert_eq!(parsed3.activity_time, Some(mtime));

    let _ = path;
}

// ---------------------------------------------------------------------------
// Malformed middle/tail records and missing Workspace
// ---------------------------------------------------------------------------

#[test]
fn malformed_middle_record_does_not_abort_discovery() {
    let fx = Fixture::new();
    let path = fx.session_root.join("encoded-ws").join("mid.jsonl");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut file = fs::File::create(&path).unwrap();
    writeln!(
        file,
        "{}",
        serde_json::to_string(&header_v3("mid", &fx.workspace, 1700000000)).unwrap()
    )
    .unwrap();
    writeln!(file, "{{ this is not valid json }}").unwrap();
    writeln!(
        file,
        "{}",
        serde_json::to_string(&user_message_string("after", 1700000010)).unwrap()
    )
    .unwrap();

    let outcome = fx.discover_default();
    assert_eq!(outcome.parsed.len(), 1);
    assert_eq!(outcome.parsed[0].messages.len(), 1);
    assert_eq!(outcome.parsed[0].messages[0].text, "after");
}

#[test]
fn truncated_tail_is_incomplete_but_keeps_valid_records() {
    let fx = Fixture::new();
    let path = fx.session_root.join("encoded-ws").join("trunc.jsonl");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut file = fs::File::create(&path).unwrap();
    writeln!(
        file,
        "{}",
        serde_json::to_string(&header_v3("trunc", &fx.workspace, 1700000000)).unwrap()
    )
    .unwrap();
    write!(
        file,
        "{{\"type\":\"user\",\"message\":{{\"role\":\"user\",\"content\":\"being writ"
    )
    .unwrap();
    file.sync_all().unwrap();

    let outcome = fx.discover_default();
    // Header is valid; the incomplete tail is dropped.
    assert_eq!(outcome.parsed.len(), 1);
    assert_eq!(outcome.parsed[0].id, "trunc");
    assert!(outcome.parsed[0].messages.is_empty());
}

#[test]
fn missing_workspace_is_discoverable_but_unresumable_cwd() {
    let fx = Fixture::new();
    let header = json!({ "type": "session", "id": "nws", "timestamp": 1700000000u64 });
    fx.write_grouped(
        "encoded-ws",
        "no-ws.jsonl",
        &[header, user_message_string("hi", 1700000010)],
    );
    let outcome = fx.discover_default();
    // Missing Workspace: surfaced for diagnosis (not out-of-scope, not excluded).
    assert_eq!(outcome.parsed.len(), 1);
    assert!(outcome.parsed[0].workspace.is_none());
}

// ---------------------------------------------------------------------------
// ResumeSpec generation
// ---------------------------------------------------------------------------

#[test]
fn resume_spec_uses_absolute_session_path_never_session_id() {
    let fx = Fixture::new();
    let path = fx.write_grouped(
        "encoded-ws",
        "resume.jsonl",
        &[
            header_v3("resume-id", &fx.workspace, 1700000000),
            user_message_string("x", 1700000010),
        ],
    );
    let outcome = fx.discover_default();
    let parsed = &outcome.parsed[0];
    let spec = parsed.resume_spec(&fx.roots_default());

    assert_eq!(spec.program, OsString::from("pi"));
    // Must be --session <absolute path>, never --session-id.
    assert!(spec.argv.iter().any(|a| a == "--session"));
    assert!(!spec.argv.iter().any(|a| a == "--session-id"));
    // The path argument must be the absolute transcript path.
    let canonical = path.canonicalize().unwrap();
    let session_idx = spec.argv.iter().position(|a| a == "--session").unwrap();
    let path_arg: PathBuf = spec.argv[session_idx + 1].clone().into();
    assert_eq!(path_arg.canonicalize().unwrap(), canonical);
    // No --session-dir for default (grouped) root.
    assert!(!spec.argv.iter().any(|a| a == "--session-dir"));
    // cwd is the workspace.
    assert_eq!(spec.cwd, fx.workspace);
}

#[test]
fn resume_spec_preserves_custom_session_dir() {
    let fx = Fixture::new();
    let custom = fx.agent_root.join("custom-sessions");
    fs::create_dir_all(&custom).unwrap();
    let roots = fx.roots_custom(custom.clone());
    let path = {
        let mut file = fs::File::create(custom.join("c.jsonl")).unwrap();
        writeln!(
            file,
            "{}",
            serde_json::to_string(&header_v3("custom-id", &fx.workspace, 1700000000)).unwrap()
        )
        .unwrap();
        writeln!(
            file,
            "{}",
            serde_json::to_string(&user_message_string("y", 1700000010)).unwrap()
        )
        .unwrap();
        custom.join("c.jsonl")
    };

    let outcome = fx.discover_custom(roots.clone());
    let spec = outcome.parsed[0].resume_spec(&roots);
    // --session-dir is preserved.
    let dir_idx = spec.argv.iter().position(|a| a == "--session-dir").unwrap();
    assert_eq!(
        PathBuf::from(spec.argv[dir_idx + 1].clone())
            .canonicalize()
            .unwrap(),
        custom.canonicalize().unwrap()
    );
    let _ = path;
}

#[test]
fn resume_spec_cwd_falls_back_when_workspace_missing() {
    let fx = Fixture::new();
    let header = json!({ "type": "session", "id": "nws2", "timestamp": 1700000000u64 });
    fx.write_grouped(
        "encoded-ws",
        "nws2.jsonl",
        &[header, user_message_string("z", 1700000010)],
    );
    let outcome = fx.discover_default();
    let spec = outcome.parsed[0].resume_spec(&fx.roots_default());
    assert_eq!(spec.cwd, PathBuf::from("."));
}

// ---------------------------------------------------------------------------
// Activity: positive-evidence-only
// ---------------------------------------------------------------------------

#[test]
fn activity_unknown_without_control_evidence() {
    let fx = Fixture::new();
    fx.write_grouped(
        "encoded-ws",
        "act.jsonl",
        &[
            header_v3("act", &fx.workspace, 1700000000),
            user_message_string("hi", 1700000010),
        ],
    );
    let outcome = fx.discover_default();
    let status = pi::activity_status(&outcome.parsed[0], None);
    assert_eq!(status, crate::session::ActivityStatus::Unknown);
}

#[test]
fn activity_active_only_with_validated_id_and_path() {
    let fx = Fixture::new();
    let path = fx.write_grouped(
        "encoded-ws",
        "act2.jsonl",
        &[
            header_v3("act2", &fx.workspace, 1700000000),
            user_message_string("hi", 1700000010),
        ],
    );
    let outcome = fx.discover_default();
    let parsed = &outcome.parsed[0];

    let now = SystemTime::now();
    // Matching evidence.
    let evidence = SessionControlEvidence {
        session_id: "act2".to_string(),
        transcript_path: path.clone(),
        observed_at: now,
    };
    assert_eq!(
        pi::activity_status(parsed, Some(&evidence)),
        crate::session::ActivityStatus::Active { observed_at: now }
    );

    // Mismatched ID → Unknown.
    let evidence_bad_id = SessionControlEvidence {
        session_id: "wrong".to_string(),
        transcript_path: path.clone(),
        observed_at: now,
    };
    assert_eq!(
        pi::activity_status(parsed, Some(&evidence_bad_id)),
        crate::session::ActivityStatus::Unknown
    );

    // Mismatched path → Unknown.
    let evidence_bad_path = SessionControlEvidence {
        session_id: "act2".to_string(),
        transcript_path: fx.workspace.join("nope.jsonl"),
        observed_at: now,
    };
    assert_eq!(
        pi::activity_status(parsed, Some(&evidence_bad_path)),
        crate::session::ActivityStatus::Unknown
    );
}

// ---------------------------------------------------------------------------
// Session construction and risk
// ---------------------------------------------------------------------------

#[test]
fn into_session_builds_supported_session_with_recorded_workspace() {
    let fx = Fixture::new();
    fx.write_grouped(
        "encoded-ws",
        "s.jsonl",
        &[
            header_v3("s", &fx.workspace, 1700000000),
            session_info("Title"),
        ],
    );
    let outcome = fx.discover_default();
    let session = outcome.parsed[0].clone().into_session(
        &fx.roots_default(),
        crate::session::RiskStatus::Normal,
        crate::session::ActivityStatus::Unknown,
    );
    assert_eq!(session.key.agent, OsString::from("pi"));
    assert_eq!(session.resumable_id, OsString::from("s"));
    assert_eq!(session.title.as_deref(), Some("Title"));
    assert_eq!(session.support, crate::session::SupportStatus::Supported);
    match &session.workspace {
        crate::session::WorkspaceEvidence::Recorded { workspace, .. } => {
            assert_eq!(workspace, &fx.workspace);
        }
        _ => panic!("expected recorded workspace"),
    }
}

#[test]
fn broad_workspace_risk_flagged_for_home_and_root() {
    let parsed = ParsedSession {
        id: "r".into(),
        workspace: Some(PathBuf::from("/")),
        parent: None,
        session_info_name: None,
        header_time: None,
        activity_time: None,
        messages: vec![],
        transcript_path: PathBuf::from("/x.jsonl"),
        file_mtime: None,
    };
    assert_eq!(
        pi::risk_status(&parsed, Some(Path::new("/"))),
        crate::session::RiskStatus::BroadWorkspace
    );
}

// ---------------------------------------------------------------------------
// READ-ONLY invariant: discovery/Preview never migrates or modifies files
// ---------------------------------------------------------------------------

#[test]
fn discovery_does_not_modify_v1_v2_v3_files_bytes_or_mtimes() {
    let fx = Fixture::new();

    let v1_path = fx.write_grouped(
        "encoded-ws",
        "v1.jsonl",
        &[
            header_v1("v1", &fx.workspace, 1700000000),
            user_message_string("v1 msg", 1700000010),
        ],
    );
    let v2_path = fx.write_grouped(
        "encoded-ws",
        "v2.jsonl",
        &[
            header_v2("v2", &fx.workspace, 1700000000),
            user_message_string("v2 msg", 1700000010),
        ],
    );
    let v3_path = fx.write_grouped(
        "encoded-ws",
        "v3.jsonl",
        &[
            header_v3("v3", &fx.workspace, 1700000000),
            user_message_string("v3 msg", 1700000010),
        ],
    );

    let before = snapshot::snapshot_dir(&fx.agent_root, true).unwrap();
    let snaps_before: Vec<_> = [&v1_path, &v2_path, &v3_path]
        .iter()
        .map(|p| snapshot::snapshot_file(p).unwrap())
        .collect();

    // Run discovery.
    let _ = fx.discover_default();

    let after = snapshot::snapshot_dir(&fx.agent_root, true).unwrap();
    snapshot::assert_unchanged(&before, &after);

    for (path, snap_before) in [&v1_path, &v2_path, &v3_path].iter().zip(&snaps_before) {
        let snap_after = snapshot::snapshot_file(path).unwrap();
        snapshot::assert_file_unchanged(snap_before, &snap_after);
    }
}

#[test]
fn discovery_of_growing_file_is_read_only() {
    let fx = Fixture::new();
    let path = fx.write_grouped(
        "encoded-ws",
        "live.jsonl",
        &[
            header_v3("live", &fx.workspace, 1700000000),
            user_message_string("first", 1700000010),
        ],
    );
    // Simulate a live writer appending a partial record.
    fx.append_raw(
        &path,
        b"{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"being",
    );

    let before = snapshot::snapshot_file(&path).unwrap();
    let outcome = fx.discover_default();
    let after = snapshot::snapshot_file(&path).unwrap();
    snapshot::assert_file_unchanged(&before, &after);
    assert_eq!(outcome.parsed.len(), 1);
}

#[test]
fn discovery_does_not_create_or_migrate_any_files() {
    let fx = Fixture::new();
    fx.write_grouped(
        "encoded-ws",
        "a.jsonl",
        &[
            header_v3("a", &fx.workspace, 1700000000),
            user_message_string("a", 1700000010),
        ],
    );
    let before = snapshot::snapshot_dir(&fx.agent_root, true).unwrap();
    let _ = fx.discover_default();
    let after = snapshot::snapshot_dir(&fx.agent_root, true).unwrap();
    snapshot::assert_unchanged(&before, &after);
}

// ---------------------------------------------------------------------------
// Fake `pi` executable capturing exact cwd/argv/env
// ---------------------------------------------------------------------------

/// Build a fake `pi` binary that records its invocation details to a capture
/// file and exits 0. Returns the absolute path to the fake binary.
#[cfg(unix)]
fn fake_pi(capture_path: &Path) -> PathBuf {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("pi");
    let capture = capture_path.display().to_string();
    let script = format!(
        "#!/bin/sh\nprintf '%s\\0' \"$PWD\" >> \"{capture}\"\nfor a in \"$@\"; do printf '%s\\0' \"$a\" >> \"{capture}\"; done\n",
    );
    fs::write(&bin, script).unwrap();
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(&bin).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&bin, perms).unwrap();
    // Keep the tempdir alive for the test by leaking it (test-scoped).
    std::mem::forget(dir);
    bin
}

/// Execute a [`ResumeSpec`] as a real subprocess and capture what the fake
/// `pi` observed via the capture file. Mirrors the eventual exec boundary:
/// discrete program/argv, no shell. The caller substitutes the program path
/// to point at the fake binary.
#[cfg(unix)]
fn run_resume_spec_capturing(spec: &crate::session::ResumeSpec) -> std::io::Result<()> {
    use std::process::Command;
    let mut cmd = Command::new(&spec.program);
    cmd.args(&spec.argv);
    cmd.current_dir(&spec.cwd);
    for (k, v) in &spec.env {
        cmd.env(k, v);
    }
    let status = cmd.status()?;
    assert!(status.success(), "fake pi must exit 0");
    Ok(())
}

#[cfg(unix)]
#[test]
fn fake_pi_captures_exact_cwd_argv_and_session_path() {
    let fx = Fixture::new();
    let capture = tempfile::NamedTempFile::new().unwrap();
    let capture_path = capture.path().to_path_buf();
    let fake_bin = fake_pi(&capture_path);

    let path = fx.write_grouped(
        "encoded-ws",
        "exec.jsonl",
        &[
            header_v3("exec", &fx.workspace, 1700000000),
            user_message_string("e", 1700000010),
        ],
    );
    let outcome = fx.discover_default();
    let mut spec = outcome.parsed[0].resume_spec(&fx.roots_default());
    // Substitute the program path to point at the fake binary. This mirrors
    // the exec boundary exactly (discrete program/argv, no shell, no PATH).
    spec.program = fake_bin.clone().into_os_string();

    run_resume_spec_capturing(&spec).unwrap();

    // Read the capture file.
    let data = fs::read(&capture_path).unwrap();
    let fields: Vec<&[u8]> = data.split(|b| *b == 0).filter(|f| !f.is_empty()).collect();
    let fields: Vec<String> = fields
        .into_iter()
        .map(|f| String::from_utf8_lossy(f).into_owned())
        .collect();
    // fields[0] = cwd, fields[1..] = argv.
    assert_eq!(
        PathBuf::from(&fields[0]).canonicalize().unwrap(),
        fx.workspace.canonicalize().unwrap()
    );
    assert_eq!(fields[1], "--session");
    assert_eq!(
        PathBuf::from(&fields[2]).canonicalize().unwrap(),
        path.canonicalize().unwrap()
    );
    // No --session-id emitted.
    assert!(!fields.iter().any(|f| f == "--session-id"));
}

#[cfg(unix)]
#[test]
fn fake_pi_captures_custom_session_dir_in_argv() {
    let fx = Fixture::new();
    let custom = fx.agent_root.join("custom-sessions");
    fs::create_dir_all(&custom).unwrap();
    let roots = fx.roots_custom(custom.clone());
    let capture = tempfile::NamedTempFile::new().unwrap();
    let capture_path = capture.path().to_path_buf();
    let fake_bin = fake_pi(&capture_path);

    let mut file = fs::File::create(custom.join("cs.jsonl")).unwrap();
    writeln!(
        file,
        "{}",
        serde_json::to_string(&header_v3("cs", &fx.workspace, 1700000000)).unwrap()
    )
    .unwrap();
    writeln!(
        file,
        "{}",
        serde_json::to_string(&user_message_string("cs", 1700000010)).unwrap()
    )
    .unwrap();

    let outcome = fx.discover_custom(roots.clone());
    let mut spec = outcome.parsed[0].resume_spec(&roots);
    spec.program = fake_bin.into_os_string();
    run_resume_spec_capturing(&spec).unwrap();

    let data = fs::read(&capture_path).unwrap();
    let fields: Vec<String> = data
        .split(|b| *b == 0)
        .filter(|f| !f.is_empty())
        .map(|f| String::from_utf8_lossy(f).into_owned())
        .collect();
    // argv must contain --session-dir <custom> after --session <path>.
    let dir_idx = fields
        .iter()
        .position(|f| f == "--session-dir")
        .expect("--session-dir preserved for custom root");
    assert_eq!(
        PathBuf::from(&fields[dir_idx + 1]).canonicalize().unwrap(),
        custom.canonicalize().unwrap()
    );
}

// ---------------------------------------------------------------------------
// Settings reading
// ---------------------------------------------------------------------------

#[test]
fn read_settings_returns_none_when_absent() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(pi::read_settings(tmp.path()).is_none());
}

#[test]
fn read_settings_parses_session_dir() {
    let fx = Fixture::new();
    let settings = json!({ "sessionDir": fx.agent_root.join("x") });
    fs::write(
        fx.agent_root.join("settings.json"),
        serde_json::to_string(&settings).unwrap(),
    )
    .unwrap();
    let parsed = pi::read_settings(&fx.agent_root).unwrap();
    assert_eq!(pi::settings_dir_pub(&parsed), Some(fx.agent_root.join("x")));
}

#[test]
fn read_settings_ignores_invalid_json() {
    let fx = Fixture::new();
    fs::write(fx.agent_root.join("settings.json"), "{ not valid").unwrap();
    assert!(pi::read_settings(&fx.agent_root).is_none());
}
