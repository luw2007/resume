use crate::integration::omp::children::discover_children;
use serde_json::json;
use std::fs;
use std::io::Write;
use std::path::PathBuf;

fn write_jsonl(path: &std::path::Path, records: &[serde_json::Value]) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    let mut file = fs::File::create(path).unwrap();
    for record in records {
        writeln!(file, "{}", serde_json::to_string(record).unwrap()).unwrap();
    }
}

#[test]
fn discovers_child_under_parent_stem_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let session_root = tmp.path().join("agent").join("sessions");

    let ws_dir = session_root.join("-workspace-");

    // Parent file
    write_jsonl(
        &ws_dir.join("parent-session.jsonl"),
        &[
            json!({"type": "session", "id": "parent-id-1", "cwd": "/home/user/project", "timestamp": 1700000000}),
            json!({"type": "message", "role": "user", "message": {"role": "user", "content": [{"type": "text", "text": "hello"}]}, "timestamp": 1700000010}),
        ],
    );

    // Child directory matching parent stem
    write_jsonl(
        &ws_dir.join("parent-session").join("child-worker.jsonl"),
        &[
            json!({"type": "session", "id": "child-id-1", "cwd": "/home/user/project/sub", "title": "Code Worker"}),
            json!({"type": "message", "role": "user", "message": {"role": "user", "content": [{"type": "text", "text": "do work"}]}, "timestamp": 1700000020}),
        ],
    );

    let result = discover_children(&session_root);
    assert_eq!(result.children.len(), 1);
    assert!(result.diagnostics.is_empty());

    let child = &result.children[0];
    assert_eq!(child.child_id.as_deref(), Some("child-id-1"));
    assert_eq!(child.name.as_deref(), Some("Code Worker"));
    assert_eq!(child.cwd, Some(PathBuf::from("/home/user/project/sub")));
    assert!(child.has_activity);
    assert!(child.parent_locator.ends_with("parent-session.jsonl"));
}

#[test]
fn child_never_becomes_a_session() {
    let tmp = tempfile::tempdir().unwrap();
    let session_root = tmp.path().join("agent").join("sessions");
    let ws_dir = session_root.join("-workspace-");

    // Parent file
    write_jsonl(
        &ws_dir.join("my-session.jsonl"),
        &[json!({"type": "session", "id": "sess-1", "cwd": "/work", "timestamp": 1700000000})],
    );

    // Child directory
    write_jsonl(
        &ws_dir.join("my-session").join("worker.jsonl"),
        &[json!({"type": "session", "id": "child-1", "cwd": "/work", "timestamp": 1700000001})],
    );

    // Child discovery finds it as ChildExecution (not Session)
    let children = discover_children(&session_root);
    assert_eq!(children.children.len(), 1);
    assert_eq!(children.children[0].child_id.as_deref(), Some("child-1"));

    // ChildExecution has no resumable_id, no ResumeSpec — structurally
    // distinct from Session. This is enforced at the type level.
    let child = &children.children[0];
    assert!(child.parent_locator.ends_with("my-session.jsonl"));
}

#[test]
fn malformed_child_isolated_as_diagnostic() {
    let tmp = tempfile::tempdir().unwrap();
    let session_root = tmp.path().join("sessions");
    let ws_dir = session_root.join("-ws-");

    // Parent
    write_jsonl(
        &ws_dir.join("p.jsonl"),
        &[json!({"type": "session", "id": "pid", "cwd": "/w"})],
    );

    // Malformed child
    let child_path = ws_dir.join("p").join("bad.jsonl");
    fs::create_dir_all(child_path.parent().unwrap()).unwrap();
    fs::write(&child_path, "{{not json\ntruncated").unwrap();

    let result = discover_children(&session_root);
    // Graceful: either diagnostic (IO parse error) or child with no activity, no panic
    if !result.children.is_empty() {
        assert!(
            result.children.iter().any(|c| !c.has_activity) || !result.diagnostics.is_empty(),
            "malformed file detected via has_activity=false or diagnostic"
        );
    } else {
        assert!(!result.diagnostics.is_empty(), "malformed file produced diagnostic");
    }
}

#[test]
fn child_with_import_badge_preserves_structured_metadata() {
    let tmp = tempfile::tempdir().unwrap();
    let session_root = tmp.path().join("sessions");
    let ws_dir = session_root.join("-ws-");

    // Parent
    write_jsonl(
        &ws_dir.join("imported.jsonl"),
        &[json!({"type": "session", "id": "imp-parent", "cwd": "/w", "timestamp": 1700000000})],
    );

    // Child with foreign_session_import
    write_jsonl(
        &ws_dir.join("imported").join("foreign-child.jsonl"),
        &[json!({
            "type": "session",
            "id": "foreign-child-id",
            "cwd": "/w/child",
            "foreign_session_import": {
                "source_kind": "claude",
                "origin_id": "orig-uuid-1234",
                "origin_cwd": "/original/path"
            }
        })],
    );

    let result = discover_children(&session_root);
    assert_eq!(result.children.len(), 1);
    let child = &result.children[0];
    assert_eq!(child.child_id.as_deref(), Some("foreign-child-id"));

    // Import badge preserved as structured data
    let badge = child.import.as_ref().expect("import badge present");
    assert_eq!(badge.source_kind, "claude");
    assert_eq!(badge.origin_id.as_deref(), Some("orig-uuid-1234"));
    assert_eq!(
        badge.origin_cwd,
        Some(PathBuf::from("/original/path"))
    );
}
