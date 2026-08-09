#![allow(unused_imports)]
use crate::integration::pi::test_support::*;
use crate::{
    integration::pi::{
        self, DiscoverConfig, EffectiveRoots, ParsedSession, ResolutionInputs,
        SessionControlEvidence,
    },
    preview::snapshot,
    scope::{Direction, Scope},
};
use serde_json::json;
use std::{
    ffi::OsString,
    fs,
    io::Write,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};
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
        crate::preview::message::Attachment::Image { media_type, .. } => {
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
