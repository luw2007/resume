use crate::integration::claude::children::discover_children;
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
fn discovers_subagent_with_parent_session_id() {
    let tmp = tempfile::tempdir().unwrap();
    let projects = tmp.path().join("projects");
    let ws_dir = projects.join("-workspace-key");
    let parent_uuid = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";

    // Parent transcript
    write_jsonl(
        &ws_dir.join(format!("{parent_uuid}.jsonl")),
        &[json!({
            "type": "user",
            "sessionId": parent_uuid,
            "cwd": "/home/user/work",
            "message": {"role": "user", "content": "hello"}
        })],
    );

    // Subagent transcript with explicit parent link
    write_jsonl(
        &ws_dir.join("subagents").join("child-001.jsonl"),
        &[
            json!({
                "type": "user",
                "parentSessionId": parent_uuid,
                "sessionId": "child-session-id",
                "cwd": "/home/user/work/sub",
                "agentName": "code-reviewer",
                "message": {"role": "user", "content": "review this"}
            }),
        ],
    );

    let result = discover_children(&projects);
    assert_eq!(result.children.len(), 1);
    assert!(result.diagnostics.is_empty());

    let child = &result.children[0];
    assert_eq!(child.parent_id, parent_uuid);
    assert_eq!(child.agent_id.as_deref(), Some("child-session-id"));
    assert_eq!(child.name.as_deref(), Some("code-reviewer"));
    assert_eq!(child.cwd, Some(PathBuf::from("/home/user/work/sub")));
    assert!(child.has_activity);
}

#[test]
fn subagent_without_parent_field_uses_single_parent_fallback() {
    let tmp = tempfile::tempdir().unwrap();
    let projects = tmp.path().join("projects");
    let ws_dir = projects.join("-workspace-key");
    let parent_uuid = "11111111-2222-3333-4444-555555555555";

    // Single parent transcript
    write_jsonl(
        &ws_dir.join(format!("{parent_uuid}.jsonl")),
        &[json!({
            "type": "user",
            "sessionId": parent_uuid,
            "cwd": "/work"
        })],
    );

    // Subagent without parentSessionId
    write_jsonl(
        &ws_dir.join("subagents").join("agent.jsonl"),
        &[json!({
            "type": "user",
            "sessionId": "sub-id",
            "cwd": "/work/child"
        })],
    );

    let result = discover_children(&projects);
    assert_eq!(result.children.len(), 1);
    // Falls back to the single parent UUID
    assert_eq!(result.children[0].parent_id, parent_uuid);
}

#[test]
fn children_never_appear_in_top_level_sessions() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let root = crate::integration::claude::resolve_root(None, Some(home)).unwrap();
    let cwd = home.join("work");
    fs::create_dir_all(&cwd).unwrap();

    let parent_uuid = "aaaaaaaa-1111-2222-3333-444444444444";
    let root_dir = home.join(".claude");

    // Parent
    let ws_key = "-workspace";
    let ws_dir = root_dir.join("projects").join(ws_key);
    write_jsonl(
        &ws_dir.join(format!("{parent_uuid}.jsonl")),
        &[json!({
            "type": "user",
            "sessionId": parent_uuid,
            "cwd": cwd.to_str().unwrap(),
            "message": {"role": "user", "content": "parent msg"}
        })],
    );

    // Subagent
    write_jsonl(
        &ws_dir.join("subagents").join("child.jsonl"),
        &[json!({
            "type": "user",
            "sessionId": "child-id",
            "cwd": cwd.to_str().unwrap(),
            "message": {"role": "user", "content": "child msg"}
        })],
    );

    // Top-level discovery must NOT include the child
    let discovery = crate::integration::claude::discover(&root).unwrap();
    assert_eq!(discovery.sessions.len(), 1);
    assert_eq!(
        discovery.sessions[0].resumable_id,
        std::ffi::OsString::from(parent_uuid)
    );

    // Child discovery finds it
    let children =
        discover_children(&root_dir.join("projects"));
    assert_eq!(children.children.len(), 1);
    assert_eq!(children.children[0].parent_id, parent_uuid);
}

#[test]
fn malformed_child_transcript_isolated_as_diagnostic() {
    let tmp = tempfile::tempdir().unwrap();
    let projects = tmp.path().join("projects");
    let ws_dir = projects.join("-key");

    // Parent
    write_jsonl(
        &ws_dir.join("parent.jsonl"),
        &[json!({"type": "user", "sessionId": "p1", "cwd": "/w"})],
    );

    // Malformed: write invalid JSON
    let malformed_path = ws_dir.join("subagents").join("bad.jsonl");
    fs::create_dir_all(malformed_path.parent().unwrap()).unwrap();
    fs::write(&malformed_path, "not valid json\n{broken").unwrap();

    let result = discover_children(&projects);
    // Malformed child is handled gracefully — either as a diagnostic (IO error)
    // or as a child with has_activity=false (parseable file, no valid records)
    if !result.children.is_empty() {
        // If it was parseable despite being "malformed", it should have no activity
        assert!(
            result.children.iter().any(|c| !c.has_activity) || !result.diagnostics.is_empty(),
            "malformed file detected via has_activity=false or diagnostic"
        );
    } else {
        // File was unparseable → diagnostic
        assert!(!result.diagnostics.is_empty(), "malformed file produced diagnostic");
    }
}
