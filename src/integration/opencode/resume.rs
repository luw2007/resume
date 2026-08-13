use std::path::PathBuf;

use crate::session::{Diagnostic, ResumeSpec};

use super::ParsedSession;

/// Build the exact Resume spec for an OpenCode session.
///
/// `opencode --session <id>` selects and resumes exactly this session; the
/// process is launched with `cwd` set to the recorded Workspace so
/// OpenCode's own directory-derived state (its `project` row) agrees with
/// what `resume` displayed.
pub fn resume_spec(parsed: &ParsedSession) -> Result<ResumeSpec, Diagnostic> {
    Ok(ResumeSpec {
        program: super::AGENT.into(),
        argv: vec!["--session".into(), parsed.id.clone().into()],
        cwd: parsed.directory.clone(),
        env: Vec::new(),
    })
}

/// Path the caller should watch for staleness (revalidation): the shared
/// session database. OpenCode has no per-session transcript file, so the
/// whole database's identity is the closest available staleness signal;
/// this coarsens revalidation (any session update makes every launched
/// Resume look "changed") in exchange for never claiming a false negative.
pub fn transcript_path(effective_root: &std::path::Path) -> PathBuf {
    super::roots::db_path(effective_root)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;

    fn fake_opencode(capture_path: &std::path::Path) -> PathBuf {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("opencode");
        let capture = capture_path.display().to_string();
        let script = format!(
            "#!/bin/sh\nprintf '%s\\0' \"$PWD\" >> \"{capture}\"\nfor a in \"$@\"; do printf '%s\\0' \"$a\" >> \"{capture}\"; done\n",
        );
        fs::write(&bin, script).unwrap();
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&bin).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&bin, perms).unwrap();
        std::mem::forget(dir);
        bin
    }

    /// Execute a [`ResumeSpec`] as a real subprocess and capture what the
    /// fake `opencode` observed. Mirrors the eventual exec boundary:
    /// discrete program/argv, no shell.
    fn run_resume_spec_capturing(spec: &ResumeSpec) -> std::io::Result<()> {
        let mut cmd = Command::new(&spec.program);
        cmd.args(&spec.argv);
        cmd.current_dir(&spec.cwd);
        cmd.env_clear();
        for (k, v) in &spec.env {
            cmd.env(k, v);
        }
        let status = cmd.status()?;
        assert!(status.success(), "fake opencode must exit 0");
        Ok(())
    }

    #[test]
    fn fake_opencode_captures_exact_cwd_and_session_argv() {
        let workspace = tempfile::tempdir().unwrap();
        let parsed = ParsedSession {
            id: "ses_6b367071dffeA7tz8mGcyzZfEI".to_string(),
            directory: workspace.path().to_path_buf(),
            title: Some("Fix the bug".to_string()),
            updated_at: None,
        };
        let capture = tempfile::NamedTempFile::new().unwrap();
        let capture_path = capture.path().to_path_buf();
        let fake_bin = fake_opencode(&capture_path);

        let mut spec = resume_spec(&parsed).unwrap();
        spec.program = fake_bin.into_os_string();
        run_resume_spec_capturing(&spec).unwrap();

        let data = fs::read(&capture_path).unwrap();
        let fields: Vec<String> = data
            .split(|b| *b == 0)
            .filter(|f| !f.is_empty())
            .map(|f| String::from_utf8_lossy(f).into_owned())
            .collect();
        // fields[0] = cwd, fields[1..] = argv.
        assert_eq!(
            PathBuf::from(&fields[0]).canonicalize().unwrap(),
            workspace.path().canonicalize().unwrap()
        );
        assert_eq!(fields[1], "--session");
        assert_eq!(fields[2], parsed.id);
        // No --continue/--fork emitted: they would select the wrong session.
        assert!(!fields.iter().any(|f| f == "--continue" || f == "--fork"));
    }
}
