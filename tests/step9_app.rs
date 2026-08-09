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
    run_with_env(home, workspace, args, &[])
}

fn run_with_env(
    home: &Path,
    workspace: &Path,
    args: &[&str],
    extra_env: &[(&str, &Path)],
) -> std::process::Output {
    let bin = home.join("bin");
    fs::create_dir_all(&bin).unwrap();
    for agent in ["pi", "claude", "codex", "omp"] {
        executable(&bin.join(agent), "#!/bin/sh\nexit 0\n");
    }
    let xdg = home.join("xdg");
    for directory in ["config", "data", "state", "cache"] {
        fs::create_dir_all(xdg.join(directory)).unwrap();
    }
    let mut command = Command::new(binary());
    command
        .args(args)
        .current_dir(workspace)
        // Clear the inherited environment so integration tests cannot observe
        // runner credentials, agent roots, config, or executable search paths.
        .env_clear()
        .env("HOME", home)
        // Load-bearing for deterministic activity assertions: do not inherit a
        // host `lsof`, which could correlate fixtures with unrelated processes.
        .env("PATH", &bin)
        .env("TERM", "dumb")
        .env("RESUME_DISABLE_PROC_PROBE", "1")
        .env("XDG_CONFIG_HOME", xdg.join("config"))
        .env("XDG_DATA_HOME", xdg.join("data"))
        .env("XDG_STATE_HOME", xdg.join("state"))
        .env("XDG_CACHE_HOME", xdg.join("cache"))
        .env("PI_CODING_AGENT_DIR", home.join(".pi/agent"))
        .env("PI_CONFIG_DIR", home.join(".omp"))
        .env("CLAUDE_CONFIG_DIR", home.join(".claude"))
        .env("CODEX_HOME", home.join(".codex"));
    for (key, value) in extra_env {
        command.env(key, value);
    }
    command.output().unwrap()
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

/// `docs/product-design.md` Â§7: "list mode rejects confirmation
/// options" and "`config example` and `completions` reject Session-query
/// options". Exercised end-to-end through the compiled binary so it covers
/// `main`'s `Cli::validate()` wiring, not just the unit-level parser.
#[test]
fn meaningless_option_combinations_are_usage_errors() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path().join("workspace");
    fs::create_dir(&ws).unwrap();
    for args in [
        vec!["--list", "--confirm-always"],
        vec!["--list", "--no-confirm"],
        vec!["--json", "--confirm-always"],
        vec!["--up", "1", "config", "example"],
        vec!["-a", "codex", "completions", "bash"],
    ] {
        let output = run(tmp.path(), &ws, &args);
        assert_eq!(output.status.code(), Some(2), "{args:?}");
    }
    // Sanity: the same subcommands without Session-query options, and
    // ordinary --list/--json without confirmation options, still succeed.
    for args in [
        vec!["config", "example"],
        vec!["completions", "bash"],
        vec!["--list"],
    ] {
        let output = run(tmp.path(), &ws, &args);
        assert!(output.status.success(), "{args:?}: {output:?}");
    }
}

#[test]
fn codex_corrupt_rollout_is_diagnosed_while_valid_sibling_survives() {
    let (tmp, ws) = fixtures();
    let corrupt_dir = tmp.path().join(".codex/sessions/2026/01/01");
    for name in [
        "rollout-corrupt.jsonl",
        "rollout-corrupt-2.jsonl",
        "rollout-corrupt-3.jsonl",
    ] {
        fs::write(corrupt_dir.join(name), "not-json").unwrap();
    }

    let output = run(tmp.path(), &ws, &["--json", "--agent", "codex"]);
    assert!(output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        value["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|session| { session["id"] == "codex-id" })
    );
    assert!(
        value["errors"]
            .as_array()
            .unwrap()
            .iter()
            .any(|error| { error["category"] == "codex_invalid_session" })
    );
    let matching: Vec<_> = value["errors"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|error| error["category"] == "codex_invalid_session")
        .collect();
    assert_eq!(matching.len(), 1);
    assert_eq!(matching[0]["count"], 3);
    assert!(String::from_utf8_lossy(&output.stderr).contains("codex_invalid_session: 3"));

    let list = run(tmp.path(), &ws, &["--list", "--agent", "codex"]);
    assert!(String::from_utf8_lossy(&list.stderr).contains("codex_invalid_session: 3"));
}

#[test]
fn disabled_process_probe_never_invokes_lsof_and_leaves_codex_unknown() {
    let (tmp, ws) = fixtures();
    let marker = tmp.path().join("lsof-invoked");
    fs::create_dir_all(tmp.path().join("bin")).unwrap();
    executable(
        &tmp.path().join("bin/lsof"),
        &format!("#!/bin/sh\ntouch '{}'\nexit 0\n", marker.display()),
    );

    let output = run(tmp.path(), &ws, &["--json", "--agent", "codex"]);
    assert!(output.status.success());
    assert!(!marker.exists(), "disabled probe invoked fake lsof");
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["sessions"][0]["activity"], "Unknown");
}

#[test]
fn unreadable_sole_integration_store_is_a_failed_integration() {
    for (agent, store, category) in [
        ("codex", ".codex/sessions", "codex_root_unavailable"),
        ("claude", ".claude/projects", "claude_root_unavailable"),
    ] {
        let (tmp, ws) = fixtures();
        let store = tmp.path().join(store);
        let original = fs::metadata(&store).unwrap().permissions();
        let mut unreadable = original.clone();
        unreadable.set_mode(0o000);
        fs::set_permissions(&store, unreadable).unwrap();

        // Root/administrator test runners may retain read access despite mode 000.
        if fs::read_dir(&store).is_ok() {
            fs::set_permissions(&store, original).unwrap();
            continue;
        }
        let output = run(tmp.path(), &ws, &["--json", "--agent", agent]);
        fs::set_permissions(&store, original).unwrap();

        assert_eq!(output.status.code(), Some(1), "{agent}: {output:?}");
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        let matching: Vec<_> = value["errors"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|error| error["category"] == category)
            .collect();
        assert_eq!(matching.len(), 1, "{agent}: {value}");
        assert_eq!(matching[0]["count"], 1, "{agent}: {value}");
    }
}

#[test]
fn unreadable_one_of_multiple_integration_stores_preserves_partial_success() {
    let (tmp, ws) = fixtures();
    let store = tmp.path().join(".codex/sessions");
    let original = fs::metadata(&store).unwrap().permissions();
    let mut unreadable = original.clone();
    unreadable.set_mode(0o000);
    fs::set_permissions(&store, unreadable).unwrap();
    if fs::read_dir(&store).is_ok() {
        fs::set_permissions(&store, original).unwrap();
        return;
    }

    let output = run(tmp.path(), &ws, &["--json"]);
    fs::set_permissions(&store, original).unwrap();
    assert!(output.status.success(), "{output:?}");
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        value["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["agent"] == "pi")
    );
    assert!(
        value["errors"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| { e["category"] == "codex_root_unavailable" && e["count"] == 1 })
    );
}

#[cfg(unix)]
#[test]
fn codex_cross_root_symlink_rejection_is_diagnosed() {
    use std::os::unix::fs::symlink;

    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path().join("workspace");
    fs::create_dir(&ws).unwrap();
    let codex_dir = tmp.path().join(".codex/sessions/2026/01/01");
    fs::create_dir_all(&codex_dir).unwrap();
    let outside = tempfile::tempdir().unwrap();
    let transcript = outside.path().join("evil.jsonl");
    line(
        &transcript,
        serde_json::json!({"type":"session_meta","payload":{"id":"EVIL","cwd":ws}}),
    );
    symlink(&transcript, codex_dir.join("rollout-evil.jsonl")).unwrap();

    let output = run(
        tmp.path(),
        &ws,
        &["--verbose", "--json", "--agent", "codex"],
    );
    assert!(output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(value["sessions"].as_array().unwrap().is_empty());
    assert!(
        value["errors"]
            .as_array()
            .unwrap()
            .iter()
            .any(|error| { error["category"] == "codex_io" })
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("codex_io"));
}

#[test]
fn omp_import_badge_is_visible_without_origin_secrets() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path().join("workspace");
    fs::create_dir(&ws).unwrap();
    let omp_dir = tmp.path().join(".omp/agent");
    fs::create_dir_all(&omp_dir).unwrap();
    let transcript = omp_dir.join("import.jsonl");
    line(
        &transcript,
        serde_json::json!({"type":"title","v":1,"title":"Imported Session"}),
    );
    line(
        &transcript,
        serde_json::json!({"type":"session","version":3,"id":"omp-import","timestamp":1700000000,"cwd":ws}),
    );
    line(
        &transcript,
        serde_json::json!({"type":"custom","foreign_session_import":{"source_kind":"codex","origin_id":"1234567890abcdef","origin_cwd":"/SECRET/PATH"}}),
    );

    let output = run_with_env(
        tmp.path(),
        &ws,
        &["--json", "--agent", "omp"],
        &[("PI_CODING_AGENT_DIR", omp_dir.as_path())],
    );
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    let value: serde_json::Value = serde_json::from_str(&text).unwrap();
    let title = value["sessions"][0]["title"]
        .as_str()
        .unwrap_or_else(|| panic!("missing title in {value}"));
    assert!(title.contains("imported from codex origin:12345678"));
    assert!(!text.contains("1234567890abcdef"));
    assert!(!text.contains("/SECRET/PATH"));
}

#[cfg(feature = "codex-sqlite")]
#[test]
fn codex_sqlite_precedence_diagnostic_reaches_json_and_stderr() {
    use rusqlite::Connection;

    let (tmp, ws) = fixtures();
    let rollout = tmp
        .path()
        .join(".codex/sessions/2026/01/01/rollout-test.jsonl");
    let db = tmp.path().join(".codex/state_5.sqlite");
    let conn = Connection::open(db).unwrap();
    conn.execute_batch(
        "CREATE TABLE sessions (
            rollout_path TEXT PRIMARY KEY,
            thread_id TEXT,
            cwd TEXT,
            title TEXT,
            updated_at TEXT,
            archived INTEGER DEFAULT 0
        );",
    )
    .unwrap();
    conn.execute(
        "INSERT INTO sessions VALUES (?1, 'WRONG-ID', ?2, 'wrong title', NULL, 0)",
        rusqlite::params![rollout.to_str().unwrap(), ws.to_str().unwrap()],
    )
    .unwrap();
    drop(conn);

    let output = run(
        tmp.path(),
        &ws,
        &["--verbose", "--json", "--agent", "codex"],
    );
    assert!(output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["sessions"][0]["id"], "codex-id");
    assert_ne!(value["sessions"][0]["title"], "wrong title");
    assert!(
        value["errors"]
            .as_array()
            .unwrap()
            .iter()
            .any(|error| { error["category"] == "codex_sqlite_id_mismatch" })
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("codex_sqlite_id_mismatch"));
}

#[test]
fn malformed_since_value_is_usage_error() {
    let (tmp, ws) = fixtures();
    assert_eq!(
        run(tmp.path(), &ws, &["--list", "--since", "yesterday"])
            .status
            .code(),
        Some(2)
    );
}

#[test]
fn since_all_matches_since_flag_absent() {
    let (tmp, ws) = fixtures();
    let with_all = run(tmp.path(), &ws, &["--json", "--since", "all"]);
    let without_flag = run(tmp.path(), &ws, &["--json"]);
    assert!(with_all.status.success());
    assert!(without_flag.status.success());
    let with_all: serde_json::Value = serde_json::from_slice(&with_all.stdout).unwrap();
    let without_flag: serde_json::Value = serde_json::from_slice(&without_flag.stdout).unwrap();
    assert_eq!(
        with_all["sessions"].as_array().unwrap().len(),
        without_flag["sessions"].as_array().unwrap().len()
    );
}

#[test]
fn since_duration_filters_out_stale_transcripts_across_all_four_agents() {
    let (tmp, ws) = fixtures();
    // The fixture transcripts were just written, so they are within the last
    // minute; `--since 0m` (an implausibly narrow window achieved by backdating
    // every transcript file's mtime) should filter them all out, while
    // `--since all` keeps them. We backdate file mtimes directly rather than
    // sleeping in the test, since the filter reads mtime from disk.
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
    for entry in walk_jsonl(tmp.path()) {
        let file = fs::OpenOptions::new().write(true).open(&entry).unwrap();
        file.set_times(fs::FileTimes::new().set_modified(old))
            .unwrap();
    }

    let recent = run(tmp.path(), &ws, &["--json", "--since", "10m"]);
    assert!(recent.status.success());
    let recent: serde_json::Value = serde_json::from_slice(&recent.stdout).unwrap();
    assert_eq!(
        recent["sessions"].as_array().unwrap().len(),
        0,
        "all transcripts are older than 10m, so --since 10m must exclude them all: {recent}"
    );

    let everything = run(tmp.path(), &ws, &["--json", "--since", "all"]);
    assert!(everything.status.success());
    let everything: serde_json::Value = serde_json::from_slice(&everything.stdout).unwrap();
    assert_eq!(
        everything["sessions"].as_array().unwrap().len(),
        4,
        "--since all must not filter anything: {everything}"
    );

    let wide = run(tmp.path(), &ws, &["--json", "--since", "2h"]);
    assert!(wide.status.success());
    let wide: serde_json::Value = serde_json::from_slice(&wide.stdout).unwrap();
    assert_eq!(
        wide["sessions"].as_array().unwrap().len(),
        4,
        "--since 2h must include transcripts backdated by 1h: {wide}"
    );
}

/// Collect every `.jsonl` transcript file under `home` (recursively), so the
/// test can backdate every fixture's mtime regardless of which agent wrote
/// it, without hardcoding each integration's directory layout twice.
fn walk_jsonl(home: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|ext| ext == "jsonl") {
                out.push(path);
            }
        }
    }
    let mut out = Vec::new();
    walk(home, &mut out);
    out
}
