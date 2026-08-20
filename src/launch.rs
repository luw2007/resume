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
#[derive(Debug)]
pub(crate) enum CmuxHandoffError {
    IncompleteEnv(&'static str),
    OriginUnavailable(io::Error),
    CliUnavailable,
    CliPathUnavailable,
    IdentifySpawn(io::Error),
    IdentifyStatus {
        status: std::process::ExitStatus,
        stderr: String,
    },
    IdentifyJson(&'static str),
    CallerMismatch,
    ListSpawn(io::Error),
    ListStatus {
        status: std::process::ExitStatus,
        stderr: String,
    },
    ListJson(&'static str),
    WorkspaceNotUnique {
        count: usize,
    },
    PreStateMismatch {
        expected: PathBuf,
        actual: String,
    },
    TargetUnavailable(io::Error),
    NonUtf8Target,
    ReportSpawn(io::Error),
    ReportStatus {
        status: std::process::ExitStatus,
        stderr: String,
    },
    ReadbackSpawn(io::Error),
    ReadbackStatus {
        status: std::process::ExitStatus,
        stderr: String,
    },
    ReadbackJson(&'static str),
    ReadbackMismatch {
        expected: String,
        actual: String,
    },
}

#[cfg(unix)]
impl std::fmt::Display for CmuxHandoffError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use CmuxHandoffError::*;
        match self {
            IncompleteEnv(_) => f.write_str("incomplete cmux provenance"),
            OriginUnavailable(_) => f.write_str("current directory unavailable"),
            CliUnavailable => f.write_str("cmux CLI unavailable"),
            CliPathUnavailable => f.write_str("cmux CLI path unavailable"),
            IdentifySpawn(_) => f.write_str("cmux identify could not start"),
            IdentifyStatus { .. } => f.write_str("cmux identify failed"),
            IdentifyJson(_) => f.write_str("invalid cmux identify response"),
            CallerMismatch => f.write_str("cmux caller mismatch"),
            ListSpawn(_) => f.write_str("cmux workspace list could not start"),
            ListStatus { .. } => f.write_str("cmux workspace list failed"),
            ListJson(_) => f.write_str("invalid cmux workspace list response"),
            WorkspaceNotUnique { .. } => f.write_str("cmux caller workspace is not unique"),
            PreStateMismatch { .. } => f.write_str("cmux caller workspace directory mismatch"),
            TargetUnavailable(_) => f.write_str("target Workspace unavailable"),
            NonUtf8Target => f.write_str("target Workspace is not valid UTF-8"),
            ReportSpawn(_) => f.write_str("cmux workspace report could not start"),
            ReportStatus { .. } => f.write_str("cmux workspace report failed"),
            ReadbackSpawn(_) => f.write_str("cmux workspace read-back could not start"),
            ReadbackStatus { .. } => f.write_str("cmux workspace read-back failed"),
            ReadbackJson(_) => f.write_str("invalid cmux read-back response"),
            ReadbackMismatch { .. } => f.write_str("cmux workspace read-back mismatch"),
        }
    }
}

#[cfg(unix)]
trait CmuxRunner {
    fn run(&self, program: Option<&Path>, args: &[&OsStr]) -> io::Result<std::process::Output>;
}

#[cfg(unix)]
struct ProcessCmuxRunner;
#[cfg(unix)]
impl CmuxRunner for ProcessCmuxRunner {
    fn run(&self, program: Option<&Path>, args: &[&OsStr]) -> io::Result<std::process::Output> {
        let mut command = Command::new(program.unwrap_or_else(|| Path::new("cmux")));
        command.args(args);
        command.output()
    }
}

#[cfg(unix)]
fn handoff_with(
    workspace: Option<&OsStr>,
    surface: Option<&OsStr>,
    origin: &Path,
    target: &Path,
    runner: &dyn CmuxRunner,
) -> Result<(), CmuxHandoffError> {
    let (Some(w), Some(s)) = (workspace, surface) else {
        return if workspace.is_none() && surface.is_none() {
            Ok(())
        } else {
            Err(CmuxHandoffError::IncompleteEnv("missing ID"))
        };
    };
    if w.is_empty() || s.is_empty() {
        return Err(CmuxHandoffError::IncompleteEnv("empty ID"));
    }
    let (Some(w), Some(s)) = (w.to_str(), s.to_str()) else {
        return Err(CmuxHandoffError::IncompleteEnv("non-UTF-8 ID"));
    };
    let origin = std::fs::canonicalize(origin).map_err(CmuxHandoffError::OriginUnavailable)?;
    let identify = runner
        .run(
            None,
            &[
                OsStr::new("identify"),
                OsStr::new("--json"),
                OsStr::new("--id-format"),
                OsStr::new("uuids"),
                OsStr::new("--workspace"),
                OsStr::new(w),
                OsStr::new("--surface"),
                OsStr::new(s),
            ],
        )
        .map_err(CmuxHandoffError::IdentifySpawn)?;
    if !identify.status.success() {
        return Err(CmuxHandoffError::IdentifyStatus {
            status: identify.status,
            stderr: bounded_stderr(&identify.stderr),
        });
    }
    let value: serde_json::Value = serde_json::from_slice(&identify.stdout)
        .map_err(|_| CmuxHandoffError::IdentifyJson("json"))?;
    let caller = value
        .get("caller")
        .and_then(|v| v.as_object())
        .ok_or(CmuxHandoffError::CallerMismatch)?;
    if !eq_id(caller.get("workspace_id"), w) || !eq_id(caller.get("surface_id"), s) {
        return Err(CmuxHandoffError::CallerMismatch);
    }
    let cli = value
        .get("app_cli_path")
        .and_then(|v| v.as_str())
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
        .ok_or(CmuxHandoffError::CliPathUnavailable)?;
    if !executable(&cli) {
        return Err(CmuxHandoffError::CliPathUnavailable);
    }
    let pre = list_workspaces(&cli, runner).map_err(|e| e.0)?;
    let actual = one_workspace(&pre, w)?;
    let actual_path =
        std::fs::canonicalize(actual).map_err(|_| CmuxHandoffError::PreStateMismatch {
            expected: origin.clone(),
            actual: actual.to_string(),
        })?;
    if actual_path != origin {
        return Err(CmuxHandoffError::PreStateMismatch {
            expected: origin,
            actual: actual.to_string(),
        });
    }
    let target = std::fs::canonicalize(target).map_err(CmuxHandoffError::TargetUnavailable)?;
    let target = target
        .to_str()
        .ok_or(CmuxHandoffError::NonUtf8Target)?
        .to_owned();
    let params = serde_json::json!({"workspace_id": w, "surface_id": s, "path": target});
    let report = runner
        .run(
            Some(&cli),
            &[
                OsStr::new("rpc"),
                OsStr::new("surface.report_pwd"),
                OsStr::new(&params.to_string()),
            ],
        )
        .map_err(CmuxHandoffError::ReportSpawn)?;
    if !report.status.success() {
        return Err(CmuxHandoffError::ReportStatus {
            status: report.status,
            stderr: bounded_stderr(&report.stderr),
        });
    }
    let post = list_workspaces(&cli, runner).map_err(|e| e.0)?;
    let actual = one_workspace(&post, w)?;
    if actual != target {
        return Err(CmuxHandoffError::ReadbackMismatch {
            expected: target,
            actual: actual.to_owned(),
        });
    }
    Ok(())
}

#[cfg(unix)]
fn eq_id(value: Option<&serde_json::Value>, expected: &str) -> bool {
    value
        .and_then(|v| v.as_str())
        .is_some_and(|v| v.eq_ignore_ascii_case(expected))
}
#[cfg(unix)]
fn bounded_stderr(bytes: &[u8]) -> String {
    String::from_utf8_lossy(&bytes[..bytes.len().min(1024)])
        .trim()
        .to_owned()
}
#[cfg(unix)]
fn list_workspaces(
    cli: &Path,
    runner: &dyn CmuxRunner,
) -> Result<serde_json::Value, (CmuxHandoffError, ())> {
    let out = runner
        .run(
            Some(cli),
            &[
                OsStr::new("workspace"),
                OsStr::new("list"),
                OsStr::new("--json"),
                OsStr::new("--id-format"),
                OsStr::new("uuids"),
            ],
        )
        .map_err(|e| (CmuxHandoffError::ListSpawn(e), ()))?;
    if !out.status.success() {
        return Err((
            CmuxHandoffError::ListStatus {
                status: out.status,
                stderr: bounded_stderr(&out.stderr),
            },
            (),
        ));
    }
    serde_json::from_slice(&out.stdout).map_err(|_| (CmuxHandoffError::ListJson("json"), ()))
}
#[cfg(unix)]
fn one_workspace<'a>(value: &'a serde_json::Value, id: &str) -> Result<&'a str, CmuxHandoffError> {
    let entries = value
        .get("workspaces")
        .and_then(|v| v.as_array())
        .ok_or(CmuxHandoffError::ListJson("workspaces"))?;
    let matches: Vec<&serde_json::Value> =
        entries.iter().filter(|v| eq_id(v.get("id"), id)).collect();
    if matches.len() != 1 {
        return Err(CmuxHandoffError::WorkspaceNotUnique {
            count: matches.len(),
        });
    }
    matches[0]
        .get("current_directory")
        .and_then(|v| v.as_str())
        .ok_or(CmuxHandoffError::ListJson("current_directory"))
}

#[cfg(unix)]
pub(crate) fn handoff_cmux_workspace(spec: &ResumeSpec) -> Result<(), CmuxHandoffError> {
    let runner = ProcessCmuxRunner;
    handoff_with(
        std::env::var_os("CMUX_WORKSPACE_ID").as_deref(),
        std::env::var_os("CMUX_SURFACE_ID").as_deref(),
        &std::env::current_dir().map_err(CmuxHandoffError::OriginUnavailable)?,
        &spec.cwd,
        &runner,
    )
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
        struct NoRunner;
        impl CmuxRunner for NoRunner {
            fn run(&self, _: Option<&Path>, _: &[&OsStr]) -> io::Result<std::process::Output> {
                panic!("cmux must not be invoked")
            }
        }
        assert!(
            handoff_with(
                None,
                None,
                Path::new("/origin"),
                Path::new("/target"),
                &NoRunner
            )
            .is_ok()
        );
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
