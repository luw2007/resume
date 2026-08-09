#![allow(unused_imports)]
use crate::integration::pi::test_support::*;
use crate::{
    integration::pi::{
        self, DiscoverConfig, EffectiveRoots, ParsedSession, ResolutionInputs,
        SessionControlEvidence,
    },
    scope::{Direction, Scope},
    snapshot,
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
    cmd.env_clear();
    cmd.env("HOME", &spec.cwd);
    cmd.env("XDG_CONFIG_HOME", spec.cwd.join(".xdg-config"));
    cmd.env("XDG_DATA_HOME", spec.cwd.join(".xdg-data"));
    cmd.env("XDG_STATE_HOME", spec.cwd.join(".xdg-state"));
    cmd.env("XDG_CACHE_HOME", spec.cwd.join(".xdg-cache"));
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
