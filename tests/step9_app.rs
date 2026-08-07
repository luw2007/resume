#![cfg(unix)]

use std::{
    fs,
    io::Write,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
};
use tempfile::TempDir;

fn binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_resume"))
}
fn executable(path: &Path, body: &str) {
    fs::write(path, body).unwrap();
    let mut p = fs::metadata(path).unwrap().permissions();
    p.set_mode(0o755);
    fs::set_permissions(path, p).unwrap();
}
fn run(home: &Path, workspace: &Path, args: &[&str]) -> std::process::Output {
    let bin = home.join("bin");
    fs::create_dir_all(&bin).unwrap();
    for agent in ["pi", "claude", "codex", "omp"] {
        executable(&bin.join(agent), "#!/bin/sh\nexit 0\n");
    }
    Command::new(binary())
        .args(args)
        .current_dir(workspace)
        .env("HOME", home)
        .env("PATH", &bin)
        .env_remove("PI_CODING_AGENT_DIR")
        .env_remove("PI_CODING_AGENT_SESSION_DIR")
        .env_remove("CLAUDE_CONFIG_DIR")
        .env_remove("CODEX_HOME")
        .env_remove("PI_CONFIG_DIR")
        .output()
        .unwrap()
}
fn line(path: &Path, value: serde_json::Value) {
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    writeln!(file, "{value}").unwrap();
}
fn fixtures() -> (TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let ws = home.join("workspace");
    fs::create_dir_all(&ws).unwrap();
    let pi_dir = home.join(".pi/agent/sessions/ws");
    fs::create_dir_all(&pi_dir).unwrap();
    let pi = pi_dir.join("pi.jsonl");
    line(
        &pi,
        serde_json::json!({"type":"session","version":3,"id":"pi-id","timestamp":1700000000,"cwd":ws}),
    );
    line(
        &pi,
        serde_json::json!({"type":"message","message":{"role":"user","content":"pi title"}}),
    );
    let cid = "11111111-1111-1111-1111-111111111111";
    let cdir = home.join(".claude/projects/ws");
    fs::create_dir_all(&cdir).unwrap();
    let claude = cdir.join(format!("{cid}.jsonl"));
    line(
        &claude,
        serde_json::json!({"type":"user","sessionId":cid,"cwd":ws,"message":{"content":"claude title"}}),
    );
    let codex_dir = home.join(".codex/sessions/2026/01/01");
    fs::create_dir_all(&codex_dir).unwrap();
    let codex = codex_dir.join("rollout-test.jsonl");
    line(
        &codex,
        serde_json::json!({"type":"session_meta","payload":{"id":"codex-id","cwd":ws,"timestamp":"2026-01-01T00:00:00Z"}}),
    );
    line(
        &codex,
        serde_json::json!({"type":"event_msg","payload":{"type":"user_message","message":{"role":"user","content":"codex title"}}}),
    );
    let omp_dir = home.join(".omp/agent");
    fs::create_dir_all(&omp_dir).unwrap();
    let omp = omp_dir.join("omp.jsonl");
    line(
        &omp,
        serde_json::json!({"type":"title","v":1,"title":"omp title"}),
    );
    line(
        &omp,
        serde_json::json!({"type":"session","version":3,"id":"omp-id","timestamp":1700000000,"cwd":ws}),
    );
    (tmp, ws)
}

#[test]
fn json_discovers_all_four_and_stdout_is_only_schema() {
    let (tmp, ws) = fixtures();
    let output = run(tmp.path(), &ws, &["--json"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["schemaVersion"], 1);
    let sessions = value["sessions"].as_array().unwrap();
    for agent in ["pi", "claude", "codex", "omp"] {
        assert!(
            sessions.iter().any(|s| s["agent"] == agent),
            "missing {agent}: {value}"
        );
    }
    assert!(sessions.iter().all(|s| s.get("messages").is_none()));
}

#[test]
fn list_output_uses_status_agent_updated_title_branch_workspace_priority() {
    let (tmp, ws) = fixtures();
    let output = run(tmp.path(), &ws, &["--list", "--agent", "pi"]);
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.starts_with("READY     pi"));
    assert!(text.contains("unknown"));
    assert!(text.contains("pi title"));
    assert!(text.contains(" - "));
}

#[test]
fn no_sessions_is_success_and_invalid_agent_is_usage_error() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path().join("workspace");
    fs::create_dir(&ws).unwrap();
    assert!(run(tmp.path(), &ws, &["--json"]).status.success());
    assert_eq!(
        run(tmp.path(), &ws, &["--list", "--agent", "bogus"])
            .status
            .code(),
        Some(2)
    );
}
