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
    IncompleteEnv(String),
    OriginUnavailable(String),
    CliUnavailable,
    CliPathUnavailable(String),
    IdentifySpawn(String),
    IdentifyStatus { status: String, stderr: String },
    IdentifyJson(String),
    CallerMismatch,
    ListSpawn(String),
    ListStatus { status: String, stderr: String },
    ListJson(String),
    WorkspaceNotUnique { count: usize },
    PreStateMismatch { expected: String, actual: String },
    TargetUnavailable(String),
    NonUtf8Target,
    ReportSpawn(String),
    ReportStatus { status: String, stderr: String },
    ReadbackMismatch { expected: String, actual: String },
}

#[cfg(unix)]
impl std::fmt::Display for CmuxHandoffError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use CmuxHandoffError::*;
        match self {
            IncompleteEnv(reason) => write!(f, "incomplete cmux provenance: {reason}"),
            OriginUnavailable(reason) => write!(f, "current directory unavailable: {reason}"),
            CliUnavailable => f.write_str("cmux CLI unavailable"),
            CliPathUnavailable(reason) => write!(f, "cmux CLI path unavailable: {reason}"),
            IdentifySpawn(reason) => write!(f, "cmux identify could not start: {reason}"),
            IdentifyStatus { status, stderr } => {
                write!(f, "cmux identify failed ({status}): {stderr}")
            }
            IdentifyJson(reason) => write!(f, "invalid cmux identify response: {reason}"),
            CallerMismatch => f.write_str("cmux caller mismatch"),
            ListSpawn(reason) => write!(f, "cmux workspace list could not start: {reason}"),
            ListStatus { status, stderr } => {
                write!(f, "cmux workspace list failed ({status}): {stderr}")
            }
            ListJson(reason) => write!(f, "invalid cmux workspace list response: {reason}"),
            WorkspaceNotUnique { count } => {
                write!(f, "cmux caller workspace is not unique ({count} matches)")
            }
            PreStateMismatch { expected, actual } => write!(
                f,
                "cmux caller workspace directory mismatch: expected {expected}, got {actual}"
            ),
            TargetUnavailable(reason) => write!(f, "target Workspace unavailable: {reason}"),
            NonUtf8Target => f.write_str("target Workspace is not valid UTF-8"),
            ReportSpawn(reason) => write!(f, "cmux workspace report could not start: {reason}"),
            ReportStatus { status, stderr } => {
                write!(f, "cmux workspace report failed ({status}): {stderr}")
            }
            ReadbackMismatch { expected, actual } => write!(
                f,
                "cmux workspace read-back mismatch: expected {expected}, got {actual}"
            ),
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
            Err(CmuxHandoffError::IncompleteEnv("missing ID".into()))
        };
    };
    if w.is_empty() || s.is_empty() {
        return Err(CmuxHandoffError::IncompleteEnv("empty ID".into()));
    }
    let (Some(w), Some(s)) = (w.to_str(), s.to_str()) else {
        return Err(CmuxHandoffError::IncompleteEnv("non-UTF-8 ID".into()));
    };
    let origin = std::fs::canonicalize(origin)
        .map_err(|e| CmuxHandoffError::OriginUnavailable(e.to_string()))?;
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
        .map_err(|e| CmuxHandoffError::IdentifySpawn(e.to_string()))?;
    if !identify.status.success() {
        return Err(CmuxHandoffError::IdentifyStatus {
            status: identify.status.to_string(),
            stderr: bounded_stderr(&identify.stderr),
        });
    }
    let value: serde_json::Value = serde_json::from_slice(&identify.stdout)
        .map_err(|_| CmuxHandoffError::IdentifyJson("json".into()))?;
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
        .ok_or(CmuxHandoffError::CliPathUnavailable(
            "missing app_cli_path".into(),
        ))?;
    if !executable(&cli) {
        return Err(CmuxHandoffError::CliPathUnavailable(
            cli.display().to_string(),
        ));
    }
    let pre = list_workspaces(&cli, runner).map_err(|e| e.0)?;
    let actual = one_workspace(&pre, w)?;
    let actual_path =
        std::fs::canonicalize(actual).map_err(|_| CmuxHandoffError::PreStateMismatch {
            expected: origin.display().to_string(),
            actual: actual.to_string(),
        })?;
    if actual_path != origin {
        return Err(CmuxHandoffError::PreStateMismatch {
            expected: origin.display().to_string(),
            actual: actual.to_string(),
        });
    }
    if target.to_str().is_none() {
        return Err(CmuxHandoffError::NonUtf8Target);
    }
    let target = std::fs::canonicalize(target)
        .map_err(|e| CmuxHandoffError::TargetUnavailable(e.to_string()))?;
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
        .map_err(|e| CmuxHandoffError::ReportSpawn(e.to_string()))?;
    if !report.status.success() {
        return Err(CmuxHandoffError::ReportStatus {
            status: report.status.to_string(),
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
        .map_err(|e| (CmuxHandoffError::ListSpawn(e.to_string()), ()))?;
    if !out.status.success() {
        return Err((
            CmuxHandoffError::ListStatus {
                status: out.status.to_string(),
                stderr: bounded_stderr(&out.stderr),
            },
            (),
        ));
    }
    serde_json::from_slice(&out.stdout).map_err(|_| (CmuxHandoffError::ListJson("json".into()), ()))
}
#[cfg(unix)]
fn one_workspace<'a>(value: &'a serde_json::Value, id: &str) -> Result<&'a str, CmuxHandoffError> {
    let entries = value
        .get("workspaces")
        .and_then(|v| v.as_array())
        .ok_or(CmuxHandoffError::ListJson("workspaces".into()))?;
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
        .ok_or(CmuxHandoffError::ListJson("current_directory".into()))
}

#[cfg(unix)]
pub(crate) fn handoff_cmux_workspace(spec: &ResumeSpec) -> Result<(), CmuxHandoffError> {
    let workspace = std::env::var_os("CMUX_WORKSPACE_ID");
    let surface = std::env::var_os("CMUX_SURFACE_ID");
    if workspace.is_none() && surface.is_none() {
        return Ok(());
    }
    if !command_available(OsStr::new("cmux")) {
        return Err(CmuxHandoffError::CliUnavailable);
    }
    let runner = ProcessCmuxRunner;
    handoff_with(
        workspace.as_deref(),
        surface.as_deref(),
        &std::env::current_dir().map_err(|e| CmuxHandoffError::OriginUnavailable(e.to_string()))?,
        &spec.cwd,
        &runner,
    )
}

#[cfg(unix)]
fn handoff_then_run_with<F, G>(spec: &ResumeSpec, handoff: F, run: G) -> Result<(), io::Error>
where
    F: FnOnce(&ResumeSpec) -> Result<(), CmuxHandoffError>,
    G: FnOnce(&ResumeSpec) -> Result<(), io::Error>,
{
    handoff(spec)
        .map_err(|error| io::Error::other(format!("cmux workspace handoff failed: {error}")))?;
    run(spec)
}

#[cfg(unix)]
pub(crate) fn handoff_then_exec(spec: &ResumeSpec) -> io::Error {
    match handoff_then_run_with(spec, handoff_cmux_workspace, |_| Err(exec(spec))) {
        Ok(()) => io::Error::other("native exec unexpectedly returned"),
        Err(error) => error,
    }
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
    use std::ffi::OsString;
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
    struct MockRunner {
        outputs: std::sync::Mutex<Vec<std::process::Output>>,
        calls: std::sync::Mutex<Vec<(Option<PathBuf>, Vec<String>)>>,
    }
    #[cfg(unix)]
    impl MockRunner {
        fn new(outputs: Vec<std::process::Output>) -> Self {
            Self {
                outputs: std::sync::Mutex::new(outputs),
                calls: std::sync::Mutex::new(Vec::new()),
            }
        }
    }
    #[cfg(unix)]
    impl CmuxRunner for MockRunner {
        fn run(&self, program: Option<&Path>, args: &[&OsStr]) -> io::Result<std::process::Output> {
            self.calls.lock().unwrap().push((
                program.map(Path::to_path_buf),
                args.iter()
                    .map(|a| a.to_string_lossy().into_owned())
                    .collect(),
            ));
            self.outputs
                .lock()
                .unwrap()
                .pop()
                .ok_or_else(|| io::Error::other("no fixture"))
        }
    }
    #[cfg(unix)]
    fn output(json: &str, success: bool) -> std::process::Output {
        use std::os::unix::process::ExitStatusExt;
        std::process::Output {
            status: std::process::ExitStatus::from_raw(if success { 0 } else { 1 }),
            stdout: json.as_bytes().to_vec(),
            stderr: b"fixture failure".to_vec(),
        }
    }
    #[cfg(unix)]
    #[test]
    fn production_entry_both_absent_is_noop() {
        let old_w = std::env::var_os("CMUX_WORKSPACE_ID");
        let old_s = std::env::var_os("CMUX_SURFACE_ID");
        restore_env("CMUX_WORKSPACE_ID", None);
        restore_env("CMUX_SURFACE_ID", None);
        let spec = ResumeSpec {
            program: OsString::from("missing-agent"),
            argv: vec![],
            cwd: PathBuf::from("/"),
            env: vec![],
        };
        assert!(handoff_cmux_workspace(&spec).is_ok());
        restore_env("CMUX_WORKSPACE_ID", old_w);
        restore_env("CMUX_SURFACE_ID", old_s);
    }

    #[cfg(unix)]
    #[test]
    fn handoff_then_exec_short_circuits_and_orders() {
        let spec = ResumeSpec {
            program: OsString::from("agent"),
            argv: vec![],
            cwd: PathBuf::from("/target"),
            env: vec![],
        };
        let order = std::sync::Mutex::new(Vec::new());
        let error = handoff_then_run_with(
            &spec,
            |_spec| {
                order.lock().unwrap().push("handoff");
                Ok(())
            },
            |_spec| {
                order.lock().unwrap().push("exec");
                Err(io::Error::other("agent failed"))
            },
        );
        assert_eq!(error.unwrap_err().to_string(), "agent failed");
        assert_eq!(*order.lock().unwrap(), vec!["handoff", "exec"]);
        let order = std::sync::Mutex::new(Vec::new());
        let error = handoff_then_run_with(
            &spec,
            |_spec| {
                order.lock().unwrap().push("handoff");
                Err(CmuxHandoffError::ReportStatus {
                    status: "1".into(),
                    stderr: "failed".into(),
                })
            },
            |_spec| {
                order.lock().unwrap().push("exec");
                Err(io::Error::other("must not run"))
            },
        );
        assert!(
            error
                .unwrap_err()
                .to_string()
                .contains("cmux workspace handoff failed")
        );
        assert_eq!(*order.lock().unwrap(), vec!["handoff"]);
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

    #[cfg(unix)]
    fn identify_json(path: &Path, w: &str, s: &str) -> String {
        format!(
            r#"{{"caller":{{"workspace_id":"{w}","surface_id":"{s}"}},"focused":{{"workspace_id":"{w}","surface_id":"{s}"}},"app_cli_path":"{}"}}"#,
            path.display()
        )
    }
    #[cfg(unix)]
    fn list_json(id: &str, cwd: &str) -> String {
        format!(r#"{{"workspaces":[{{"id":"{id}","current_directory":"{cwd}"}}]}}"#)
    }
    #[cfg(unix)]
    fn valid_runner(
        path: &Path,
        origin: &Path,
        _target: &Path,
        report_ok: bool,
        readback: &str,
    ) -> MockRunner {
        MockRunner::new(vec![
            output(&list_json("W", readback), true),
            output("{}", report_ok),
            output(
                &list_json("W", &origin.canonicalize().unwrap().display().to_string()),
                true,
            ),
            output(&identify_json(path, "W", "S"), true),
        ])
    }
    #[cfg(unix)]
    fn invoke(runner: &MockRunner, origin: &Path, target: &Path) -> Result<(), CmuxHandoffError> {
        handoff_with(
            Some(OsStr::new("W")),
            Some(OsStr::new("S")),
            origin,
            target,
            runner,
        )
    }
    #[cfg(unix)]
    #[test]
    fn cmux_handoff_protocol_regression_matrix() {
        let origin = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        let cli = std::env::current_exe().unwrap();
        let target_canonical = target.path().canonicalize().unwrap().display().to_string();
        let runner = valid_runner(&cli, origin.path(), target.path(), true, &target_canonical);
        assert!(
            handoff_with(
                Some(OsStr::new("W")),
                Some(OsStr::new("S")),
                origin.path(),
                target.path(),
                &runner
            )
            .is_ok()
        );
        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls.len(), 4);
        assert_eq!(calls[0].0, None);
        assert_eq!(calls[0].1[0], "identify");
        assert_eq!(calls[1].0.as_deref(), Some(cli.as_path()));
        assert_eq!(calls[2].0.as_deref(), Some(cli.as_path()));
        assert_eq!(calls[3].0.as_deref(), Some(cli.as_path()));
        assert_eq!(calls[2].1[..2], ["rpc", "surface.report_pwd"]);
        let params: serde_json::Value = serde_json::from_str(&calls[2].1[2]).unwrap();
        assert_eq!(params["workspace_id"], "W");
        assert_eq!(params["surface_id"], "S");
        assert_eq!(params["path"], target_canonical);
    }
    #[cfg(unix)]
    #[test]
    fn cmux_handoff_rejects_incomplete_and_malformed_states() {
        let dir = tempfile::tempdir().unwrap();
        for (workspace, surface) in [
            (Some(OsStr::new("W")), None),
            (None, Some(OsStr::new("S"))),
            (Some(OsStr::new("")), Some(OsStr::new("S"))),
            (Some(OsStr::new("W")), Some(OsStr::new(""))),
        ] {
            let runner = MockRunner::new(vec![]);
            assert!(matches!(
                handoff_with(workspace, surface, dir.path(), dir.path(), &runner),
                Err(CmuxHandoffError::IncompleteEnv(_))
            ));
            assert_eq!(runner.calls.lock().unwrap().len(), 0);
        }
        let bad = MockRunner::new(vec![output("not json", true)]);
        assert!(matches!(
            handoff_with(
                Some(OsStr::new("W")),
                Some(OsStr::new("S")),
                dir.path(),
                dir.path(),
                &bad
            ),
            Err(CmuxHandoffError::IdentifyJson(_))
        ));
        let path = std::env::current_exe().unwrap();
        for list in ["not json", r#"{"unexpected":[]}"#] {
            let runner = MockRunner::new(vec![
                output(list, true),
                output(&identify_json(&path, "W", "S"), true),
            ]);
            assert!(matches!(
                invoke(&runner, dir.path(), dir.path()),
                Err(CmuxHandoffError::ListJson(_))
            ));
            assert_eq!(runner.calls.lock().unwrap().len(), 2);
        }
    }
    #[cfg(unix)]
    #[test]
    fn cmux_handoff_rejects_mismatch_duplicate_and_failures() {
        let origin = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        let path = std::env::current_exe().unwrap();
        for identify in [
            identify_json(&path, "OTHER", "S"),
            identify_json(&path, "W", "OTHER"),
        ] {
            let mismatch = MockRunner::new(vec![output(&identify, true)]);
            assert!(matches!(
                invoke(&mismatch, origin.path(), target.path()),
                Err(CmuxHandoffError::CallerMismatch)
            ));
            assert_eq!(mismatch.calls.lock().unwrap().len(), 1);
        }
        for workspaces in [
            r#"{"workspaces":[]}"#,
            r#"{"workspaces":[{"id":"W","current_directory":"/x"},{"id":"W","current_directory":"/x"}]}"#,
        ] {
            let runner = MockRunner::new(vec![
                output(workspaces, true),
                output(&identify_json(&path, "W", "S"), true),
            ]);
            assert!(matches!(
                invoke(&runner, origin.path(), target.path()),
                Err(CmuxHandoffError::WorkspaceNotUnique { .. })
            ));
            assert_eq!(runner.calls.lock().unwrap().len(), 2);
        }
        let pre = MockRunner::new(vec![
            output(&list_json("W", "/different"), true),
            output(&identify_json(&path, "W", "S"), true),
        ]);
        assert!(matches!(
            invoke(&pre, origin.path(), target.path()),
            Err(CmuxHandoffError::PreStateMismatch { .. })
        ));
        assert_eq!(pre.calls.lock().unwrap().len(), 2);
        let id = identify_json(&path, "W", "S");
        let report = MockRunner::new(vec![
            output("{}", false),
            output(&list_json("W", &origin.path().display().to_string()), true),
            output(&id, true),
        ]);
        assert!(matches!(
            handoff_with(
                Some(OsStr::new("W")),
                Some(OsStr::new("S")),
                origin.path(),
                target.path(),
                &report
            ),
            Err(CmuxHandoffError::ReportStatus { .. })
        ));
        assert_eq!(report.calls.lock().unwrap().len(), 3);
        let readback = valid_runner(
            &path,
            origin.path(),
            target.path(),
            true,
            &origin.path().display().to_string(),
        );
        assert!(matches!(
            invoke(&readback, origin.path(), target.path()),
            Err(CmuxHandoffError::ReadbackMismatch { .. })
        ));
        assert_eq!(readback.calls.lock().unwrap().len(), 4);
        let canonical = target.path().canonicalize().unwrap().display().to_string();
        let trailing = valid_runner(
            &path,
            origin.path(),
            target.path(),
            true,
            &(canonical + "/"),
        );
        assert!(matches!(
            invoke(&trailing, origin.path(), target.path()),
            Err(CmuxHandoffError::ReadbackMismatch { .. })
        ));
    }
    #[cfg(unix)]
    #[test]
    fn cmux_handoff_rejects_missing_and_nonexecutable_app_cli_path() {
        let origin = tempfile::tempdir().unwrap();
        let missing = MockRunner::new(vec![output(
            r#"{"caller":{"workspace_id":"W","surface_id":"S"}}"#,
            true,
        )]);
        assert!(matches!(
            invoke(&missing, origin.path(), origin.path()),
            Err(CmuxHandoffError::CliPathUnavailable(_))
        ));
        assert_eq!(missing.calls.lock().unwrap().len(), 1);
        let file = origin.path().join("not-executable");
        std::fs::write(&file, "#!/bin/sh\n").unwrap();
        let json = format!(
            r#"{{"caller":{{"workspace_id":"W","surface_id":"S"}},"app_cli_path":"{}"}}"#,
            file.display()
        );
        let nonexec = MockRunner::new(vec![output(&json, true)]);
        assert!(matches!(
            invoke(&nonexec, origin.path(), origin.path()),
            Err(CmuxHandoffError::CliPathUnavailable(_))
        ));
        assert_eq!(nonexec.calls.lock().unwrap().len(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn fake_cmux_and_native_agent_prove_order_and_fail_closed_exec() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let origin = root.path().join("origin");
        let target = root.path().join("target");
        std::fs::create_dir_all(&origin).unwrap();
        std::fs::create_dir_all(&target).unwrap();
        let state = root.path().join("state");
        let log = root.path().join("log");
        std::fs::write(&state, origin.canonicalize().unwrap().display().to_string()).unwrap();
        let cmux = root.path().join("cmux");
        let script = format!(
            r##"#!/bin/sh
printf '%s\n' "$*" >> "{}"
if [ "$1" = identify ]; then printf '{{"caller":{{"workspace_id":"W","surface_id":"S"}},"app_cli_path":"{}"}}\n'
elif [ "$1" = workspace ]; then current=$(<{}); printf '%s' "{{\"workspaces\":[{{\"id\":\"W\",\"current_directory\":\"$current\"}}]}}"
elif [ "$1" = rpc ]; then if [ "${{FAIL_REPORT:-0}}" = 1 ]; then exit 1; fi; printf '%s' "{}" > '{}'; fi
"##,
            log.display(),
            cmux.display(),
            state.display(),
            target.canonicalize().unwrap().display(),
            state.display()
        );
        std::fs::write(&cmux, script).unwrap();
        let mut p = std::fs::metadata(&cmux).unwrap().permissions();
        p.set_mode(0o755);
        std::fs::set_permissions(&cmux, p).unwrap();
        let agent = root.path().join("agent");
        let marker = root.path().join("marker");
        std::fs::write(
            &agent,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$PWD\" > \"{}\"\n",
                marker.display()
            ),
        )
        .unwrap();
        let mut p = std::fs::metadata(&agent).unwrap().permissions();
        p.set_mode(0o755);
        std::fs::set_permissions(&agent, p).unwrap();
        let old_path = std::env::var_os("PATH");
        let old_w = std::env::var_os("CMUX_WORKSPACE_ID");
        let old_s = std::env::var_os("CMUX_SURFACE_ID");
        let old_cwd = std::env::current_dir().unwrap();
        unsafe {
            std::env::set_var("PATH", root.path());
            std::env::set_var("CMUX_WORKSPACE_ID", "W");
            std::env::set_var("CMUX_SURFACE_ID", "S");
        }
        std::env::set_current_dir(&origin).unwrap();
        let spec = ResumeSpec {
            program: agent.clone().into_os_string(),
            argv: vec![],
            cwd: target.clone(),
            env: vec![],
        };
        let success = handoff_then_run_with(&spec, handoff_cmux_workspace, |spec| {
            Command::new(&spec.program)
                .current_dir(&spec.cwd)
                .status()
                .map(|_| ())
        });
        assert!(
            success.is_ok(),
            "production handoff/exec failed: {success:?}"
        );
        assert_eq!(
            std::fs::read_to_string(&marker).unwrap().trim(),
            target.canonicalize().unwrap().display().to_string()
        );
        let count = std::fs::read_to_string(&log)
            .unwrap()
            .matches("rpc")
            .count();
        std::fs::remove_file(&marker).unwrap();
        std::fs::write(&state, origin.canonicalize().unwrap().display().to_string()).unwrap();
        unsafe {
            std::env::set_var("FAIL_REPORT", "1");
        }
        let failed_report = handoff_then_run_with(&spec, handoff_cmux_workspace, |spec| {
            Command::new(&spec.program)
                .current_dir(&spec.cwd)
                .status()
                .map(|_| ())
        });
        assert!(
            failed_report
                .unwrap_err()
                .to_string()
                .contains("cmux workspace handoff failed")
        );
        assert!(!marker.exists());
        unsafe {
            std::env::remove_var("FAIL_REPORT");
        }
        std::fs::write(&state, origin.display().to_string()).unwrap();
        let spec_bad = ResumeSpec {
            program: root.path().join("missing-agent").into_os_string(),
            argv: vec![],
            cwd: target.clone(),
            env: vec![],
        };
        let before = std::fs::read_to_string(&log)
            .unwrap()
            .matches("rpc")
            .count();
        let failed_exec = handoff_then_run_with(&spec_bad, handoff_cmux_workspace, |spec| {
            Command::new(&spec.program)
                .current_dir(&spec.cwd)
                .status()
                .map(|_| ())
        });
        assert!(
            !failed_exec
                .unwrap_err()
                .to_string()
                .contains("cmux workspace handoff failed")
        );
        assert_eq!(before, count + 1);
        std::env::set_current_dir(old_cwd).unwrap();
        restore_env("PATH", old_path);
        restore_env("CMUX_WORKSPACE_ID", old_w);
        restore_env("CMUX_SURFACE_ID", old_s);
    }

    #[cfg(unix)]
    #[test]
    fn production_entry_rejects_missing_cmux_cli() {
        let old_path = std::env::var_os("PATH");
        let old_w = std::env::var_os("CMUX_WORKSPACE_ID");
        let old_s = std::env::var_os("CMUX_SURFACE_ID");
        unsafe {
            std::env::set_var("PATH", tempfile::tempdir().unwrap().path());
            std::env::set_var("CMUX_WORKSPACE_ID", "W");
            std::env::set_var("CMUX_SURFACE_ID", "S");
        }
        let spec = ResumeSpec {
            program: OsString::from("agent"),
            argv: vec![],
            cwd: PathBuf::from("/"),
            env: vec![],
        };
        assert!(matches!(
            handoff_cmux_workspace(&spec),
            Err(CmuxHandoffError::CliUnavailable)
        ));
        restore_env("PATH", old_path);
        restore_env("CMUX_WORKSPACE_ID", old_w);
        restore_env("CMUX_SURFACE_ID", old_s);
    }
    #[cfg(unix)]
    fn restore_env(key: &str, value: Option<OsString>) {
        unsafe {
            if let Some(value) = value {
                std::env::set_var(key, value);
            } else {
                std::env::remove_var(key);
            }
        }
    }
    #[cfg(unix)]
    #[test]
    fn cmux_handoff_covers_target_and_symlink_guards() {
        use std::os::unix::ffi::OsStringExt;
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        let real = root.path().join("real");
        let alias = root.path().join("alias");
        let target = root.path().join("target");
        std::fs::create_dir_all(&real).unwrap();
        std::fs::create_dir_all(&target).unwrap();
        symlink(&real, &alias).unwrap();
        let cli = std::env::current_exe().unwrap();
        let runner = valid_runner(
            &cli,
            &real,
            &target,
            true,
            &target.canonicalize().unwrap().display().to_string(),
        );
        assert!(invoke(&runner, &alias, &target).is_ok());
        let bad_target = root.path().join(OsString::from_vec(b"bad-\xff".to_vec()));
        let pre = MockRunner::new(vec![
            output(&list_json("W", &real.display().to_string()), true),
            output(&identify_json(&cli, "W", "S"), true),
        ]);
        assert!(matches!(
            handoff_with(
                Some(OsStr::new("W")),
                Some(OsStr::new("S")),
                &real,
                &bad_target,
                &pre
            ),
            Err(CmuxHandoffError::NonUtf8Target)
        ));
        assert_eq!(pre.calls.lock().unwrap().len(), 2);
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
