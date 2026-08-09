#![allow(unused_imports)]
use crate::integration::claude::test_support::*;
use crate::{
    integration::claude,
    jsonl,
    session::{ActivityStatus, SupportStatus, WorkspaceEvidence},
    snapshot,
};
use std::{
    ffi::{OsStr, OsString},
    fs,
    path::{Path, PathBuf},
};
mod fake_claude {
    use std::{
        ffi::{OsStr, OsString},
        fs,
        path::{Path, PathBuf},
        process::Command,
    };

    /// Record of what the fake launcher observed.
    #[derive(Debug)]
    pub struct LaunchRecord {
        pub cwd: PathBuf,
        pub argv: Vec<OsString>,
        pub env: Vec<(OsString, OsString)>,
    }

    impl LaunchRecord {
        pub fn arg(&self, index: usize) -> Option<&OsStr> {
            self.argv.get(index).map(OsString::as_os_str)
        }

        pub fn env_get(&self, key: &str) -> Option<&OsStr> {
            self.env
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.as_os_str())
        }
    }

    /// Write a fake `claude` script to a temp dir and return its path plus the
    /// path to the file it will write its launch record to.
    pub fn install(tmp: &Path) -> (PathBuf, PathBuf) {
        let bin_dir = tmp.join("bin");
        fs::create_dir_all(&bin_dir).unwrap();
        let record = tmp.join("launch_record.txt");
        let record_for_script = record.clone();

        #[cfg(unix)]
        {
            // Record only the actual arguments ($@), not $0 (the program
            // name), so the parsed `argv` mirrors the ResumeSpec.argv the
            // launcher would exec with.
            let script = format!(
                r#"#!/bin/sh
                : > "{rec}"
                for a in "$@"; do echo "argv:$a" >> "{rec}"; done
                pwd >> "{rec}"
                echo "env:CLAUDE_CONFIG_DIR=${{CLAUDE_CONFIG_DIR}}" >> "{rec}"
                exit 0
                "#,
                rec = record_for_script.display()
            );
            let bin = bin_dir.join("claude");
            fs::write(&bin, script).unwrap();
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&bin).unwrap().permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&bin, perms).unwrap();
            (bin, record)
        }
        #[cfg(not(unix))]
        {
            // Non-Unix: we cannot exec a shell script. This test suite targets
            // Unix exec semantics; on other platforms the record is unused.
            let _ = record_for_script;
            (bin_dir.join("claude"), record)
        }
    }

    /// Parse a launch record file written by the fake script.
    pub fn parse_record(path: &Path) -> LaunchRecord {
        let text = fs::read_to_string(path).unwrap();
        let mut argv = Vec::new();
        let mut cwd = PathBuf::new();
        let mut env = Vec::new();
        for line in text.lines() {
            if let Some(rest) = line.strip_prefix("argv:") {
                argv.push(OsString::from(rest));
            } else if let Some(rest) = line.strip_prefix("env:") {
                if let Some((k, v)) = rest.split_once('=') {
                    env.push((OsString::from(k), OsString::from(v)));
                }
            } else if !line.is_empty() {
                cwd = PathBuf::from(line);
            }
        }
        LaunchRecord { cwd, argv, env }
    }

    /// Run the fake launcher with a ResumeSpec and return the observed record.
    #[cfg(unix)]
    pub fn run(spec: &crate::session::ResumeSpec, path: &Path) -> LaunchRecord {
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
        let status = cmd.status().expect("fake launcher must run");
        assert!(status.success(), "fake launcher exited cleanly");
        parse_record(path)
    }
}

#[cfg(unix)]
#[test]
fn fake_claude_launch_contract_captures_exact_resume_argv_cwd_env() {
    let home = tempfile::tempdir().unwrap();
    let (bin, record) = fake_claude::install(home.path());

    let root = claude::resolve_root(None, Some(home.path())).unwrap();
    let cwd = home.path().join("workspace");
    fs::create_dir_all(&cwd).unwrap();
    write_transcript(
        &default_root_dir(home.path()),
        "key",
        UUID_A,
        &standard_records(UUID_A, cwd.to_str().unwrap(), "hello"),
    );

    let discovery = claude::discover(&root).unwrap();
    let session = &discovery.sessions[0];

    // Build the ResumeSpec, then point the program at the fake launcher.
    let mut spec = claude::resume_spec(session, &root).unwrap();
    spec.program = bin.clone().into_os_string();

    let launch = fake_claude::run(&spec, &record);

    // Exact argv: `--resume <uuid>` (the program name `claude` is argv[0]
    // at exec time and is not part of ResumeSpec.argv).
    assert_eq!(launch.arg(0), Some(OsStr::new("--resume")));
    assert_eq!(launch.arg(1), Some(OsStr::new(UUID_A)));
    assert!(
        launch.argv.len() == 2,
        "no --continue, no extra flags: {:?}",
        launch.argv
    );

    // Exact cwd: the recorded Workspace. The fake launcher reports its
    // working directory via `pwd`, which resolves symlinks (e.g.
    // /var -> /private/var on macOS), so compare canonicalized forms.
    assert_eq!(
        launch.cwd.canonicalize().unwrap(),
        cwd.canonicalize().unwrap()
    );

    // Default root: no CLAUDE_CONFIG_DIR override propagated.
    assert!(
        launch
            .env_get("CLAUDE_CONFIG_DIR")
            .map(|v: &OsStr| v.is_empty())
            .unwrap_or(true),
        "default root does not propagate CLAUDE_CONFIG_DIR"
    );
}

#[cfg(unix)]
#[test]
fn fake_claude_launch_contract_preserves_nondefault_config_dir() {
    let custom = tempfile::tempdir().unwrap();
    let (bin, record) = fake_claude::install(custom.path());

    let root = claude::resolve_root(Some(custom.path().as_os_str()), None).unwrap();
    let cwd = custom.path().join("workspace");
    fs::create_dir_all(&cwd).unwrap();
    write_transcript(
        custom.path(),
        "key",
        UUID_A,
        &standard_records(UUID_A, cwd.to_str().unwrap(), "hello"),
    );

    let discovery = claude::discover(&root).unwrap();
    let session = &discovery.sessions[0];
    let mut spec = claude::resume_spec(session, &root).unwrap();
    spec.program = bin.into_os_string();

    let launch = fake_claude::run(&spec, &record);

    assert_eq!(
        launch.env_get("CLAUDE_CONFIG_DIR"),
        Some(custom.path().as_os_str()),
        "nondefault CLAUDE_CONFIG_DIR preserved on Resume"
    );
}

#[test]
fn resume_spec_rejects_missing_workspace() {
    let home = tempfile::tempdir().unwrap();
    let root = claude::resolve_root(None, Some(home.path())).unwrap();

    write_transcript(
        &default_root_dir(home.path()),
        "key",
        UUID_A,
        &[json(&[
            ("type", str_val("user")),
            ("sessionId", str_val(UUID_A)),
            (
                "message",
                json(&[("role", str_val("user")), ("content", str_val("hi"))]),
            ),
        ])],
    );

    let discovery = claude::discover(&root).unwrap();
    let session = &discovery.sessions[0];
    let err = claude::resume_spec(session, &root).unwrap_err();
    match err {
        crate::session::IntegrationError::InvalidSession { diagnostic } => {
            assert_eq!(diagnostic.category, "claude_missing_workspace");
        }
        other => panic!("expected InvalidSession, got {other:?}"),
    }
}

// ===========================================================================
// Fixture: nested subagent artifacts excluded
// ===========================================================================
