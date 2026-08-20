//! Safe post-picker revalidation, confirmation, and process replacement.

use std::{
    ffi::OsStr,
    io::{self, BufRead, Write},
    path::{Path, PathBuf},
    process::Command,
};

use crate::session::{ActivityStatus, ResumeSpec, RiskStatus, Session, SupportStatus};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RevalidationError {
    TranscriptChanged,
    CliUnavailable,
    WorkspaceUnavailable,
    WorkspaceChanged,
    Unsupported,
}

impl std::fmt::Display for RevalidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::TranscriptChanged => "transcript identity changed after selection",
            Self::CliUnavailable => "agent CLI is no longer available",
            Self::WorkspaceUnavailable => "Workspace is no longer available",
            Self::WorkspaceChanged => "Workspace was replaced after selection",
            Self::Unsupported => "Session is not supported for Resume",
        })
    }
}

/// Immutable filesystem evidence captured when a picker item is created.
#[derive(Clone, Debug)]
pub struct LaunchEvidence {
    pub transcript: PathBuf,
    pub transcript_identity: FileIdentity,
    pub workspace: PathBuf,
    pub workspace_identity: FileIdentity,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileIdentity {
    #[cfg(unix)]
    pub dev: u64,
    #[cfg(unix)]
    pub ino: u64,
    pub len: u64,
    pub modified: Option<std::time::SystemTime>,
}

impl FileIdentity {
    pub fn read(path: &Path) -> io::Result<Self> {
        let metadata = std::fs::metadata(path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            Ok(Self {
                dev: metadata.dev(),
                ino: metadata.ino(),
                len: metadata.len(),
                modified: metadata.modified().ok(),
            })
        }
        #[cfg(not(unix))]
        {
            Ok(Self {
                len: metadata.len(),
                modified: metadata.modified().ok(),
            })
        }
    }
}

impl LaunchEvidence {
    pub fn capture(session: &Session) -> io::Result<Self> {
        Self::capture_with_transcript(session, PathBuf::from(&session.key.native_locator))
    }

    /// Capture evidence when an integration's opaque native locator contains
    /// more than the transcript path (Codex includes native ID and path).
    pub fn capture_with_transcript(session: &Session, transcript: PathBuf) -> io::Result<Self> {
        let workspace = session
            .workspace
            .workspace()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "missing Workspace"))?
            .to_path_buf();
        Ok(Self {
            transcript_identity: FileIdentity::read(&transcript)?,
            workspace_identity: FileIdentity::read(&workspace)?,
            transcript,
            workspace,
        })
    }
}

pub fn command_available(program: &OsStr) -> bool {
    let path = Path::new(program);
    if path.components().count() > 1 {
        return executable(path);
    }
    std::env::var_os("PATH").is_some_and(|paths| {
        std::env::split_paths(&paths).any(|dir| executable(&dir.join(program)))
    })
}

fn executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

pub fn revalidate(
    session: &Session,
    spec: &ResumeSpec,
    evidence: &LaunchEvidence,
) -> Result<(), RevalidationError> {
    if session.support != SupportStatus::Supported {
        return Err(RevalidationError::Unsupported);
    }
    if !command_available(&spec.program) {
        return Err(RevalidationError::CliUnavailable);
    }
    let transcript = FileIdentity::read(&evidence.transcript)
        .map_err(|_| RevalidationError::TranscriptChanged)?;
    if transcript != evidence.transcript_identity {
        return Err(RevalidationError::TranscriptChanged);
    }
    let workspace =
        FileIdentity::read(&spec.cwd).map_err(|_| RevalidationError::WorkspaceUnavailable)?;
    if workspace != evidence.workspace_identity || spec.cwd != evidence.workspace {
        return Err(RevalidationError::WorkspaceChanged);
    }
    Ok(())
}

pub fn risk_reasons(session: &Session, confirm_always: bool) -> Vec<&'static str> {
    let mut reasons = Vec::new();
    if matches!(session.activity, ActivityStatus::Active { .. }) {
        reasons.push("Session is Active");
    }
    match session.risk {
        RiskStatus::BroadWorkspace => reasons.push("Workspace is broad"),
        RiskStatus::WorkspaceChanged => reasons.push("Workspace changed"),
        RiskStatus::ConflictingMetadata => reasons.push("metadata conflicts"),
        RiskStatus::Normal => {}
    }
    if confirm_always && reasons.is_empty() {
        reasons.push("confirmation requested");
    }
    reasons
}

/// Risk confirmations are mandatory: `no_confirm` suppresses only ordinary confirmation.
pub fn should_confirm(session: &Session, confirm_always: bool, no_confirm: bool) -> bool {
    let risky = !risk_reasons(session, false).is_empty();
    risky || (confirm_always && !no_confirm)
}

pub fn confirm<R: BufRead, W: Write>(
    reader: &mut R,
    writer: &mut W,
    session: &Session,
    reasons: &[&str],
) -> io::Result<bool> {
    writeln!(
        writer,
        "Resume {:?} in {}?",
        session.resumable_id,
        session
            .workspace
            .workspace()
            .map_or_else(|| "<unknown>".into(), |p| p.display().to_string())
    )?;
    if !reasons.is_empty() {
        writeln!(writer, "Risk: {}", reasons.join(", "))?;
    }
    write!(writer, "Continue? [y/N] ")?;
    writer.flush()?;
    let mut line = String::new();
    reader.read_line(&mut line)?;
    Ok(matches!(
        line.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

#[cfg(unix)]
pub fn exec(spec: &ResumeSpec) -> io::Error {
    use std::os::unix::process::CommandExt;
    let mut command = Command::new(&spec.program);
    command.args(&spec.argv).current_dir(&spec.cwd);
    for (key, value) in &spec.env {
        command.env(key, value);
    }
    command.exec()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::*;
    fn session(risk: RiskStatus, activity: ActivityStatus) -> Session {
        Session {
            key: SessionKey {
                agent: "pi".into(),
                effective_root: "/tmp".into(),
                profile: None,
                native_locator: "/tmp/t".into(),
            },
            resumable_id: "id".into(),
            title: None,
            updated_at: None,
            workspace: WorkspaceEvidence::Recorded {
                workspace: "/tmp".into(),
                historical_git_identity: None,
            },
            support: SupportStatus::Supported,
            activity,
            risk,
        }
    }
    #[cfg(unix)]
    #[test]
    fn no_cmux_env_is_noop() {
        let runner = TestRunner::default();
        assert!(handoff_with(None, None, Path::new("/origin"), Path::new("/target"), &runner).is_ok());
        assert!(runner.calls.lock().unwrap().is_empty());
    }

    #[test]
    fn no_confirm_never_bypasses_risk() {
        assert!(should_confirm(
            &session(RiskStatus::BroadWorkspace, ActivityStatus::Unknown),
            false,
            true
        ));
    }
    #[test]
    fn refusal_is_default() {
        let mut input = &b"\n"[..];
        let mut out = Vec::new();
        assert!(
            !confirm(
                &mut input,
                &mut out,
                &session(RiskStatus::Normal, ActivityStatus::Unknown),
                &[]
            )
            .unwrap()
        );
    }

    #[test]
    fn detects_transcript_and_workspace_replacement() {
        let dir = tempfile::tempdir().unwrap();
        let transcript = dir.path().join("session.jsonl");
        let workspace = dir.path().join("workspace");
        std::fs::write(&transcript, "one").unwrap();
        std::fs::create_dir(&workspace).unwrap();
        let mut session = session(RiskStatus::Normal, ActivityStatus::Unknown);
        session.key.native_locator = transcript.clone().into_os_string();
        session.workspace = WorkspaceEvidence::Recorded {
            workspace: workspace.clone(),
            historical_git_identity: None,
        };
        let evidence = LaunchEvidence::capture(&session).unwrap();
        let spec = ResumeSpec {
            program: std::env::current_exe().unwrap().into_os_string(),
            argv: vec![],
            cwd: workspace.clone(),
            env: vec![],
        };
        std::fs::write(&transcript, "changed-length").unwrap();
        assert_eq!(
            revalidate(&session, &spec, &evidence),
            Err(RevalidationError::TranscriptChanged)
        );
        std::fs::write(&transcript, "one").unwrap();
        let evidence = LaunchEvidence::capture(&session).unwrap();
        std::fs::remove_dir(&workspace).unwrap();
        std::fs::create_dir(&workspace).unwrap();
        assert_eq!(
            revalidate(&session, &spec, &evidence),
            Err(RevalidationError::WorkspaceChanged)
        );
    }

    #[test]
    fn cli_disappearing_before_exec_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let transcript = dir.path().join("session.jsonl");
        let workspace = dir.path().join("workspace");
        let cli = dir.path().join("agent");
        std::fs::write(&transcript, "one").unwrap();
        std::fs::create_dir(&workspace).unwrap();
        std::fs::write(&cli, "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut p = std::fs::metadata(&cli).unwrap().permissions();
            p.set_mode(0o755);
            std::fs::set_permissions(&cli, p).unwrap();
        }
        let mut session = session(RiskStatus::Normal, ActivityStatus::Unknown);
        session.key.native_locator = transcript.into_os_string();
        session.workspace = WorkspaceEvidence::Recorded {
            workspace: workspace.clone(),
            historical_git_identity: None,
        };
        let evidence = LaunchEvidence::capture(&session).unwrap();
        let spec = ResumeSpec {
            program: cli.clone().into_os_string(),
            argv: vec![],
            cwd: workspace,
            env: vec![],
        };
        std::fs::remove_file(cli).unwrap();
        assert_eq!(
            revalidate(&session, &spec, &evidence),
            Err(RevalidationError::CliUnavailable)
        );
    }
}
