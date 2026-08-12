use super::*;

// ===========================================================================
// ResumeSpec: default, named profile, custom session-dir, env preservation
// ===========================================================================

#[test]
fn resume_spec_default_is_resume_id() {
    let fx = Fixture::new();
    fx.write(
        &fx.default_agent_root,
        &fx.encoded_ws(),
        "r.jsonl",
        &[
            header_v3("resume-id", &fx.workspace, 1700000000),
            user_message_string("x", 1700000010),
        ],
    );
    let outcome = fx.discover(fx.roots_default());
    let spec = outcome.parsed[0].resume_spec(&fx.roots_default());

    assert_eq!(spec.program, OsString::from("omp"));
    assert_eq!(
        spec.argv,
        vec![OsString::from("--resume"), OsString::from("resume-id")]
    );
    assert_eq!(spec.cwd, fx.workspace);
}

#[test]
fn resume_spec_named_profile_adds_profile_flag() {
    let fx = Fixture::new();
    let named_root = fx.profile_agent_root("work");
    fx.write_flat(
        &named_root,
        "r.jsonl",
        &[
            header_v3("resume-id", &fx.workspace, 1700000000),
            user_message_string("x", 1700000010),
        ],
    );
    let outcome = fx.discover(fx.roots_named("work"));
    let spec = outcome.parsed[0].resume_spec(&fx.roots_named("work"));

    assert_eq!(
        spec.argv,
        vec![
            OsString::from("--profile"),
            OsString::from("work"),
            OsString::from("--resume"),
            OsString::from("resume-id"),
        ]
    );
    // No --session-dir for default (non-custom) root.
    assert!(!spec.argv.iter().any(|a| a == "--session-dir"));
}

#[test]
fn resume_spec_custom_session_dir_preserved() {
    let fx = Fixture::new();
    let custom = fx.home().join("custom-sessions");
    fs::create_dir_all(&custom).unwrap();
    let roots = fx.roots_custom(custom.clone(), ProfileSelection::Default);
    fx.write_flat(
        &custom,
        "c.jsonl",
        &[
            header_v3("custom-id", &fx.workspace, 1700000000),
            user_message_string("y", 1700000010),
        ],
    );
    let outcome = fx.discover(roots.clone());
    let spec = outcome.parsed[0].resume_spec(&roots);

    let dir_idx = spec.argv.iter().position(|a| a == "--session-dir").unwrap();
    assert_eq!(
        PathBuf::from(spec.argv[dir_idx + 1].clone())
            .canonicalize()
            .unwrap(),
        custom.canonicalize().unwrap()
    );
    // --session-dir comes after --resume <id>.
    let resume_idx = spec.argv.iter().position(|a| a == "--resume").unwrap();
    assert!(dir_idx > resume_idx);
}

#[test]
fn resume_spec_omits_default_config_root_env() {
    let fx = Fixture::new();
    fx.write(
        &fx.default_agent_root,
        &fx.encoded_ws(),
        "e.jsonl",
        &[
            header_v3("e", &fx.workspace, 1700000000),
            user_message_string("x", 1700000010),
        ],
    );
    let outcome = fx.discover(fx.roots_default());
    let spec = outcome.parsed[0].resume_spec(&fx.roots_default());
    assert!(spec.env.is_empty());
}

#[test]
fn resume_spec_preserves_explicit_config_root_env() {
    let fx = Fixture::new();
    let roots = omp::resolve(&ResolutionInputs {
        config_dir_env: Some(fx.base_root.clone()),
        ..fx.inputs_default()
    })
    .unwrap();
    fx.write(
        &roots.agent_root,
        &fx.encoded_ws(),
        "e.jsonl",
        &[
            header_v3("e", &fx.workspace, 1700000000),
            user_message_string("x", 1700000010),
        ],
    );
    let outcome = fx.discover(roots.clone());
    let spec = outcome.parsed[0].resume_spec(&roots);
    assert_eq!(
        spec.env,
        vec![(
            OsString::from("PI_CONFIG_DIR"),
            fx.base_root.clone().into_os_string(),
        )]
    );
}

#[test]
fn resume_spec_cwd_falls_back_when_workspace_missing() {
    let fx = Fixture::new();
    let header = json!({ "type": "session", "id": "nws2", "timestamp": 1700000000u64 });
    fx.write(
        &fx.default_agent_root,
        &fx.encoded_ws(),
        "nws2.jsonl",
        &[header, user_message_string("z", 1700000010)],
    );
    let outcome = fx.discover(fx.roots_default());
    let spec = outcome.parsed[0].resume_spec(&fx.roots_default());
    assert_eq!(spec.cwd, PathBuf::from("."));
}

// ===========================================================================
// FAKE `omp` LAUNCH PROVENANCE: exact cwd/argv/env
// ===========================================================================

/// Build a fake `omp` binary that records cwd + argv + PI_CONFIG_DIR to a
/// capture file and exits 0.
#[cfg(unix)]
fn fake_omp(capture_path: &Path) -> PathBuf {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("omp");
    let capture = capture_path.display().to_string();
    let script = format!(
        "#!/bin/sh\n\
         printf '%s\\0' \"$PWD\" >> \"{capture}\"\n\
         for a in \"$@\"; do printf '%s\\0' \"$a\" >> \"{capture}\"; done\n\
         printf 'PI_CONFIG_DIR=%s\\0' \"$PI_CONFIG_DIR\" >> \"{capture}\"\n",
    );
    fs::write(&bin, script).unwrap();
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(&bin).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&bin, perms).unwrap();
    std::mem::forget(dir);
    bin
}

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
    assert!(status.success(), "fake omp must exit 0");
    Ok(())
}

fn read_capture(capture_path: &Path) -> Vec<String> {
    let data = fs::read(capture_path).unwrap();
    data.split(|b| *b == 0)
        .filter(|f| !f.is_empty())
        .map(|f| String::from_utf8_lossy(f).into_owned())
        .collect()
}

#[cfg(unix)]
#[test]
fn fake_omp_captures_exact_cwd_argv_for_default_profile() {
    let fx = Fixture::new();
    let capture = tempfile::NamedTempFile::new().unwrap();
    let capture_path = capture.path().to_path_buf();
    let fake_bin = fake_omp(&capture_path);

    fx.write(
        &fx.default_agent_root,
        &fx.encoded_ws(),
        "exec.jsonl",
        &[
            header_v3("exec", &fx.workspace, 1700000000),
            user_message_string("e", 1700000010),
        ],
    );
    let outcome = fx.discover(fx.roots_default());
    let mut spec = outcome.parsed[0].resume_spec(&fx.roots_default());
    spec.program = fake_bin.into_os_string();
    run_resume_spec_capturing(&spec).unwrap();

    let fields = read_capture(&capture_path);
    // fields[0] = cwd, fields[1..] = argv, last = PI_CONFIG_DIR env line.
    assert_eq!(
        PathBuf::from(&fields[0]).canonicalize().unwrap(),
        fx.workspace.canonicalize().unwrap()
    );
    assert_eq!(fields[1], "--resume");
    assert_eq!(fields[2], "exec");
    // No default PI_CONFIG_DIR override is injected.
    assert!(fields.iter().any(|f| f == "PI_CONFIG_DIR="));
}

#[cfg(unix)]
#[test]
fn fake_omp_captures_profile_and_session_dir() {
    let fx = Fixture::new();
    let custom = fx.home().join("custom-sessions");
    fs::create_dir_all(&custom).unwrap();
    let roots = fx.roots_custom(
        custom.clone(),
        ProfileSelection::Named(OsString::from("work")),
    );
    // The named profile's agent root under the custom-session roots helper.
    fx.write_flat(
        &custom,
        "p.jsonl",
        &[
            header_v3("p", &fx.workspace, 1700000000),
            user_message_string("p", 1700000010),
        ],
    );
    let capture = tempfile::NamedTempFile::new().unwrap();
    let capture_path = capture.path().to_path_buf();
    let fake_bin = fake_omp(&capture_path);

    let outcome = fx.discover(roots.clone());
    let mut spec = outcome.parsed[0].resume_spec(&roots);
    spec.program = fake_bin.into_os_string();
    run_resume_spec_capturing(&spec).unwrap();

    let fields = read_capture(&capture_path);
    // argv = --profile work --resume p --session-dir <custom>
    assert_eq!(fields[1], "--profile");
    assert_eq!(fields[2], "work");
    assert_eq!(fields[3], "--resume");
    assert_eq!(fields[4], "p");
    assert_eq!(fields[5], "--session-dir");
    assert_eq!(
        PathBuf::from(&fields[6]).canonicalize().unwrap(),
        custom.canonicalize().unwrap()
    );
}
