use crate::integration::claude::children::discover_children_for_parents;
use crate::relation::EvidenceSource;
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
        &[json!({
            "type": "user",
            "parentSessionId": parent_uuid,
            "sessionId": "child-session-id",
            "cwd": "/home/user/work/sub",
            "agentName": "code-reviewer",
            "message": {"role": "user", "content": "review this"}
        })],
    );

    let result =
        discover_children_for_parents(&projects, &[ws_dir.join(format!("{parent_uuid}.jsonl"))]);
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

    let result =
        discover_children_for_parents(&projects, &[ws_dir.join(format!("{parent_uuid}.jsonl"))]);
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
    let children = discover_children_for_parents(
        &root_dir.join("projects"),
        &[ws_dir.join(format!("{parent_uuid}.jsonl"))],
    );
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

    let result = discover_children_for_parents(&projects, &[ws_dir.join("parent.jsonl")]);
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
        assert!(
            !result.diagnostics.is_empty(),
            "malformed file produced diagnostic"
        );
    }
}

#[test]
fn out_of_scope_workspaces_skipped_but_siblings_keep_fallback_ambiguous() {
    let tmp = tempfile::tempdir().unwrap();
    let projects = tmp.path().join("projects");
    let in_ws = projects.join("-in");
    let out_ws = projects.join("-out");
    let child = json!({"type": "user", "sessionId": "sub", "cwd": "/w"});

    write_jsonl(
        &in_ws.join("p1.jsonl"),
        &[json!({"type": "user", "sessionId": "p1"})],
    );
    write_jsonl(
        &in_ws.join("p2.jsonl"),
        &[json!({"type": "user", "sessionId": "p2"})],
    );
    write_jsonl(
        &in_ws.join("subagents").join("c.jsonl"),
        std::slice::from_ref(&child),
    );
    write_jsonl(
        &out_ws.join("q.jsonl"),
        &[json!({"type": "user", "sessionId": "q"})],
    );
    write_jsonl(&out_ws.join("subagents").join("c.jsonl"), &[child]);

    // Two top-level siblings: parentless child stays ambiguous; the
    // out-of-scope workspace is never read.
    let result =
        discover_children_for_parents(&projects, &[in_ws.join("p1.jsonl"), in_ws.join("p1.jsonl")]);
    assert!(result.children.is_empty());
    assert_eq!(result.diagnostics.len(), 1);
    assert_eq!(
        result.diagnostics[0].category,
        "claude_subagent_parent_ambiguous"
    );
}

const P1: &str = "aaaaaaaa-0000-0000-0000-000000000001";
const P2: &str = "aaaaaaaa-0000-0000-0000-000000000002";

fn parent_record(id: &str) -> serde_json::Value {
    json!({"type": "user", "sessionId": id, "cwd": "/w"})
}

#[test]
fn nested_parent_uuid_layout_is_native_layout_evidence() {
    let tmp = tempfile::tempdir().unwrap();
    let projects = tmp.path().join("projects");
    let ws = projects.join("-key");
    // Two top-level siblings: the flat fallback would be ambiguous, the
    // directory name is not.
    write_jsonl(&ws.join(format!("{P1}.jsonl")), &[parent_record(P1)]);
    write_jsonl(&ws.join(format!("{P2}.jsonl")), &[parent_record(P2)]);
    // Real native shape: sessionId is the parent's, agentId is the child's.
    write_jsonl(
        &ws.join(P1).join("subagents").join("agent-a1.jsonl"),
        &[
            json!({"type": "user", "isSidechain": true, "agentId": "a1", "sessionId": P1, "cwd": "/w"}),
        ],
    );
    write_jsonl(
        &ws.join(P2).join("subagents").join("agent-b1.jsonl"),
        &[json!({"type": "user", "agentId": "b1", "sessionId": P2, "cwd": "/w"})],
    );

    let result = discover_children_for_parents(
        &projects,
        &[
            ws.join(format!("{P2}.jsonl")),
            ws.join(format!("{P1}.jsonl")),
        ],
    );
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    assert_eq!(result.children.len(), 2);
    assert_eq!(result.children[0].parent_id, P1);
    assert_eq!(result.children[0].agent_id.as_deref(), Some("a1"));
    assert_eq!(result.children[0].source, EvidenceSource::NativeLayout);
    assert_eq!(result.children[1].parent_id, P2);
}

#[test]
fn nested_layout_only_reads_in_scope_parents() {
    let tmp = tempfile::tempdir().unwrap();
    let projects = tmp.path().join("projects");
    let ws = projects.join("-key");
    write_jsonl(&ws.join(format!("{P1}.jsonl")), &[parent_record(P1)]);
    write_jsonl(&ws.join(format!("{P2}.jsonl")), &[parent_record(P2)]);
    for p in [P1, P2] {
        write_jsonl(
            &ws.join(p).join("subagents").join("agent-x.jsonl"),
            &[json!({"type": "user", "agentId": "x", "sessionId": p})],
        );
    }
    let result = discover_children_for_parents(&projects, &[ws.join(format!("{P1}.jsonl"))]);
    assert_eq!(result.children.len(), 1);
    assert_eq!(result.children[0].parent_id, P1);
}

#[test]
fn nested_directory_conflicting_with_explicit_parent_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let projects = tmp.path().join("projects");
    let ws = projects.join("-key");
    write_jsonl(&ws.join(format!("{P1}.jsonl")), &[parent_record(P1)]);
    write_jsonl(
        &ws.join(P1).join("subagents").join("agent-x.jsonl"),
        &[json!({"type": "user", "agentId": "x", "parentSessionId": P2})],
    );
    let result = discover_children_for_parents(&projects, &[ws.join(format!("{P1}.jsonl"))]);
    assert!(result.children.is_empty());
    assert_eq!(result.diagnostics.len(), 1);
    assert_eq!(
        result.diagnostics[0].category,
        "claude_subagent_parent_conflict"
    );
}

#[test]
fn conflicting_explicit_parents_within_flat_child_are_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let projects = tmp.path().join("projects");
    let ws = projects.join("-key");
    write_jsonl(&ws.join(format!("{P1}.jsonl")), &[parent_record(P1)]);
    write_jsonl(
        &ws.join("subagents").join("c.jsonl"),
        &[
            json!({"type": "user", "parentSessionId": P1}),
            json!({"type": "user", "parent_session_id": P2}),
        ],
    );
    let result = discover_children_for_parents(&projects, &[ws.join(format!("{P1}.jsonl"))]);
    assert!(result.children.is_empty());
    assert_eq!(
        result.diagnostics[0].category,
        "claude_subagent_parent_conflict"
    );
}

#[test]
fn flat_layout_keeps_transcript_evidence() {
    let tmp = tempfile::tempdir().unwrap();
    let projects = tmp.path().join("projects");
    let ws = projects.join("-key");
    write_jsonl(&ws.join(format!("{P1}.jsonl")), &[parent_record(P1)]);
    write_jsonl(
        &ws.join("subagents").join("c.jsonl"),
        &[json!({"type": "user", "sessionId": "own"})],
    );
    let result = discover_children_for_parents(&projects, &[ws.join(format!("{P1}.jsonl"))]);
    assert_eq!(result.children[0].source, EvidenceSource::NativeTranscript);
    assert_eq!(result.children[0].agent_id.as_deref(), Some("own"));
}

#[cfg(unix)]
#[test]
fn symlinked_subagents_directories_escaping_root_are_never_listed() {
    use std::os::unix::fs::symlink;
    let tmp = tempfile::tempdir().unwrap();
    let projects = tmp.path().join("projects");
    let outside = tmp.path().join("outside");
    let ws = projects.join("-key");
    write_jsonl(&ws.join(format!("{P1}.jsonl")), &[parent_record(P1)]);
    write_jsonl(
        &outside.join("subagents").join("agent-evil.jsonl"),
        &[json!({"type": "user", "agentId": "evil", "sessionId": P1})],
    );
    // Nested subagents dir symlinked outside.
    fs::create_dir_all(ws.join(P1)).unwrap();
    symlink(outside.join("subagents"), ws.join(P1).join("subagents")).unwrap();
    // Flat subagents dir symlinked outside.
    symlink(outside.join("subagents"), ws.join("subagents")).unwrap();

    let result = discover_children_for_parents(&projects, &[ws.join(format!("{P1}.jsonl"))]);
    assert!(result.children.is_empty());
    assert!(result.diagnostics.is_empty());
}

#[cfg(unix)]
#[test]
fn symlinked_workspace_and_in_root_redirects_are_rejected() {
    use std::os::unix::fs::symlink;
    let tmp = tempfile::tempdir().unwrap();
    let projects = tmp.path().join("projects");
    let outside = tmp.path().join("outside-ws");
    write_jsonl(&outside.join(format!("{P1}.jsonl")), &[parent_record(P1)]);
    write_jsonl(
        &outside.join(P1).join("subagents").join("agent-evil.jsonl"),
        &[json!({"type": "user", "agentId": "evil"})],
    );
    fs::create_dir_all(&projects).unwrap();
    symlink(&outside, projects.join("-link")).unwrap();
    let result = discover_children_for_parents(
        &projects,
        &[projects.join("-link").join(format!("{P1}.jsonl"))],
    );
    assert!(result.children.is_empty());

    // In-root redirect: P1/subagents -> another parent's subagents.
    let ws = projects.join("-key");
    write_jsonl(&ws.join(format!("{P1}.jsonl")), &[parent_record(P1)]);
    write_jsonl(
        &ws.join(P2).join("subagents").join("agent-x.jsonl"),
        &[json!({"type": "user", "agentId": "x"})],
    );
    fs::create_dir_all(ws.join(P1)).unwrap();
    symlink(ws.join(P2).join("subagents"), ws.join(P1).join("subagents")).unwrap();
    let result = discover_children_for_parents(&projects, &[ws.join(format!("{P1}.jsonl"))]);
    assert!(result.children.is_empty());
}
