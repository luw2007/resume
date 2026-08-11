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
fn list_output_shows_update_time_and_branch_instead_of_status_and_workspace() {
    let (tmp, ws) = fixtures();
    let output = run(tmp.path(), &ws, &["--list", "--agent", "pi"]);
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(!text.starts_with("READY"));
    assert!(text.contains("pi title"));
    assert!(text.contains("+ no-branch"));
    assert!(!text.contains(&ws.display().to_string()));
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
        vec!["--all-worktrees", "--up", "1"],
        vec!["--all-worktrees", "--down", "1"],
        vec!["--all-worktrees", "config", "example"],
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
fn claude_missing_workspace_diagnostic_surfaces_instead_of_being_silently_discarded() {
    // Regression test for post-merge-review.md finding S6: discover_claude
    // used to call `claude::resume_spec(&session, &root).ok()`, silently
    // discarding the `claude_missing_workspace` diagnostic whenever a
    // Supported session had no recorded cwd anywhere in its transcript. The
    // session would still appear (unresumable, no ResumeSpec) but nothing
    // ever explained why. Fixed by threading the diagnostic into
    // `AgentDiscovery::errors` instead of discarding it.
    let (tmp, ws) = fixtures();
    let cid = "22222222-2222-2222-2222-222222222222";
    let cdir = tmp.path().join(".claude/projects/no-cwd");
    fs::create_dir_all(&cdir).unwrap();
    let claude = cdir.join(format!("{cid}.jsonl"));
    // Filename UUID agrees with the embedded sessionId (Supported identity),
    // but no record anywhere carries a `cwd` field, so the session's
    // Workspace evidence is Unknown and resume_spec must fail.
    line(
        &claude,
        serde_json::json!({"type":"user","sessionId":cid,"message":{"content":"no workspace"}}),
    );

    let output = run(tmp.path(), &ws, &["--json", "--agent", "claude"]);
    assert!(output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        value["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|session| session["id"] == cid),
        "the session itself is still discovered and shown"
    );
    assert!(
        value["errors"]
            .as_array()
            .unwrap()
            .iter()
            .any(|error| error["category"] == "claude_missing_workspace"),
        "claude_missing_workspace must surface in JSON errors, not be silently discarded"
    );

    let list = run(tmp.path(), &ws, &["--list", "--agent", "claude"]);
    assert!(
        String::from_utf8_lossy(&list.stderr).contains("claude_missing_workspace: 1"),
        "claude_missing_workspace must also surface on stderr for --list"
    );
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
fn man_flag_prints_the_manual_and_is_exclusive_with_other_arguments() {
    // cli-man-flag: `--man` must print the full manual to stdout with exit 0
    // and no config/discovery side effects, and must reject combination with
    // any other argument (clap `exclusive = true`). Previously only unit-
    // tested via `try_parse_from` (cli.rs) and `man::page()` directly
    // (man.rs), never through the real compiled binary end-to-end.
    let (tmp, ws) = fixtures();
    let man = run(tmp.path(), &ws, &["--man"]);
    assert!(man.status.success());
    let stdout = String::from_utf8(man.stdout).unwrap();
    assert!(stdout.starts_with("RESUME(1)"));
    assert!(stdout.contains("E1001"));
    assert!(stdout.contains("ERRORS"));

    let conflict = run(tmp.path(), &ws, &["--man", "--json"]);
    assert_eq!(conflict.status.code(), Some(2));
    let conflict2 = run(tmp.path(), &ws, &["--man", "/tmp"]);
    assert_eq!(conflict2.status.code(), Some(2));
}

#[test]
fn non_verbose_list_prints_a_discovery_diagnostic_without_verbose_flag() {
    // M2 fix (docs/review/post-merge-review.md): non-verbose `--list` must
    // still print discovery diagnostics to stderr; the packaging branch had
    // regressed this behind an `if options.verbose` guard. Prior coverage was
    // incidental (piggybacked on `codex_corrupt_rollout_is_diagnosed_while_
    // valid_sibling_survives`, tests/step9_app.rs:202-242 — no test is named
    // specifically for M2 per docs/qa/feature-inventory.csv's error_notes).
    // This test isolates the M2 contract with a dedicated scenario and adds
    // the stronger assertion the CSV's E2E gap called for: the diagnostic
    // line is on stderr, and diagnostic text never leaks onto stdout.
    let (tmp, ws) = fixtures();
    let corrupt_dir = tmp.path().join(".codex/sessions/2026/01/01");
    fs::write(corrupt_dir.join("rollout-corrupt.jsonl"), "not-json").unwrap();

    let list = run(tmp.path(), &ws, &["--list", "--agent", "codex"]);
    let stderr = String::from_utf8_lossy(&list.stderr);
    assert!(
        stderr.contains("codex_invalid_session: 1"),
        "expected codex_invalid_session on stderr without --verbose, got: {stderr}"
    );
    // stdout must remain the plain row list, never diagnostic text.
    let stdout = String::from_utf8_lossy(&list.stdout);
    assert!(!stdout.contains("resume:"));
    assert!(!stdout.contains("codex_invalid_session"));
}

#[test]
fn json_errors_aggregate_counts_match_stderr_across_multiple_categories() {
    // M5 fix (docs/review/post-merge-review.md): `--json`'s `errors[]` must
    // be aggregated (one entry per category, summed count), matching the
    // stderr rendering. Prior coverage was incidental and single-category
    // (Codex only, tests/step9_app.rs:230-238); this test exercises TWO
    // distinct categories (codex_invalid_session, claude_no_session_id) in
    // the same run to prove aggregation is per-category, not a single global
    // collapse, closing the gap docs/qa/feature-inventory.csv's error_notes
    // called out for m5-fix-json-errors-aggregated.
    let (tmp, ws) = fixtures();
    let codex_dir = tmp.path().join(".codex/sessions/2026/01/01");
    for name in ["rollout-bad-1.jsonl", "rollout-bad-2.jsonl"] {
        fs::write(codex_dir.join(name), "not-json").unwrap();
    }
    // Two Claude transcripts with neither an embedded sessionId nor a cwd
    // anywhere: format.rs's identity contract skips these with
    // `claude_no_session_id` (src/integration/claude/format.rs:203-214).
    let claude_dir = tmp.path().join(".claude/projects/ws");
    for uuid in [
        "22222222-2222-2222-2222-222222222222",
        "33333333-3333-3333-3333-333333333333",
    ] {
        line(
            &claude_dir.join(format!("{uuid}.jsonl")),
            serde_json::json!({"type":"user","message":{"content":"no id, no cwd"}}),
        );
    }

    let output = run(tmp.path(), &ws, &["--json"]);
    assert!(output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    let errors = value["errors"].as_array().unwrap();

    let codex_entries: Vec<_> = errors
        .iter()
        .filter(|e| e["category"] == "codex_invalid_session")
        .collect();
    assert_eq!(
        codex_entries.len(),
        1,
        "codex_invalid_session must collapse to one JSON entry: {errors:?}"
    );
    assert_eq!(codex_entries[0]["count"], 2);
    assert!(stderr.contains("codex_invalid_session: 2"));

    let claude_entries: Vec<_> = errors
        .iter()
        .filter(|e| e["category"] == "claude_no_session_id")
        .collect();
    assert_eq!(
        claude_entries.len(),
        1,
        "claude_no_session_id must collapse to one JSON entry: {errors:?}"
    );
    assert_eq!(claude_entries[0]["count"], 2);
    assert!(stderr.contains("claude_no_session_id: 2"));
}

#[test]
fn missing_base_directory_is_a_usage_error() {
    // scope-missing-base-usage-error: a nonexistent positional directory
    // argument must fail canonicalization and exit 2 with a clear message,
    // exercised through the real CLI parse (previously only implied by the
    // canonicalization unit test, never through `run()`).
    let (tmp, ws) = fixtures();
    let missing = tmp.path().join("this-directory-does-not-exist");
    let output = run(tmp.path(), &ws, &[missing.to_str().unwrap(), "--list"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(!String::from_utf8_lossy(&output.stderr).is_empty());
}

#[test]
fn git_unavailable_falls_back_to_exact_scope_with_a_visible_diagnostic() {
    // scope-git-failure-diagnostic / doccheck-git-scope-failure-not-surfaced-as-diagnostic:
    // every fixture-based test already runs with a PATH containing no `git`
    // binary (by construction of `run_with_env`), so Git Scope discovery
    // already fails in-process; this test is the first to assert the
    // resulting `git_scope_discovery_failed` diagnostic is actually visible
    // on stderr end-to-end, closing the previously-flagged "constructed but
    // never surfaced" gap.
    let (tmp, ws) = fixtures();
    let output = run(tmp.path(), &ws, &["--list", "--agent", "pi"]);
    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("git_scope_discovery_failed"),
        "expected git_scope_discovery_failed on stderr (PATH has no git), got: {stderr}"
    );
    // The exact-directory fallback still finds the pi session recorded at ws.
    assert!(String::from_utf8_lossy(&output.stdout).contains("pi title"));
}

#[test]
fn omp_discovers_default_and_every_named_profile_in_one_run() {
    // omp-all-profiles-discovered: `discover_omp`'s multi-profile enumeration
    // loop had no dedicated test — per-profile isolation was proven, but not
    // that a single invocation surfaces the default profile AND every named
    // profile together. Adds two named profiles alongside the default
    // fixture and confirms all three appear with correct `profile` fields.
    let (tmp, ws) = fixtures();
    for profile in ["work", "personal"] {
        let dir = tmp.path().join(".omp/profiles").join(profile).join("agent");
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join(format!("{profile}.jsonl"));
        line(
            &file,
            serde_json::json!({"type":"title","v":1,"title":format!("{profile} title")}),
        );
        line(
            &file,
            serde_json::json!({"type":"session","version":3,"id":format!("{profile}-id"),"timestamp":1700000000,"cwd":ws}),
        );
    }

    let omp_agent_dir = tmp.path().join(".omp/agent");
    let output = run_with_env(
        tmp.path(),
        &ws,
        &["--json", "--agent", "omp"],
        &[("PI_CODING_AGENT_DIR", omp_agent_dir.as_path())],
    );
    assert!(output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let sessions = value["sessions"].as_array().unwrap();
    assert!(
        sessions
            .iter()
            .any(|s| s["id"] == "omp-id" && s["profile"].is_null()),
        "default profile session missing: {sessions:?}"
    );
    for profile in ["work", "personal"] {
        assert!(
            sessions
                .iter()
                .any(|s| s["id"] == format!("{profile}-id") && s["profile"] == profile),
            "{profile} profile session missing: {sessions:?}"
        );
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
