use super::{
    AGENT,
    discover::{ParsedSession, extract_session},
    roots::{EffectiveRoots, settings_dir},
};
use crate::{
    preview::jsonl::{FileOutcome, ReadResult},
    session::{ActivityStatus, ResumeSpec, RiskStatus, WorkspaceEvidence},
};
use serde_json::Value;
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    time::SystemTime,
};
impl ParsedSession {
    /// Build the [`ResumeSpec`]: `pi --session <absolute-jsonl-path>`,
    /// preserving `--session-dir` when discovery used a custom root. Never
    /// uses `--session-id`.
    pub fn resume_spec(&self, roots: &EffectiveRoots) -> ResumeSpec {
        let mut argv: Vec<OsString> = Vec::with_capacity(4);
        argv.push(OsString::from("--session"));
        argv.push(absolutize(&self.transcript_path).into_os_string());
        if roots.custom_session_root {
            argv.push(OsString::from("--session-dir"));
            argv.push(absolutize(&roots.session_root).into_os_string());
        }
        let cwd = self.workspace.clone().unwrap_or_else(|| PathBuf::from("."));
        ResumeSpec {
            program: OsString::from(AGENT),
            argv,
            cwd,
            env: Vec::new(),
        }
    }
}

/// Make `path` absolute without resolving symlinks. Unlike
/// `Path::canonicalize`, this never follows a symlinked ancestor (e.g.
/// macOS's `/var` -> `/private/var`), matching Claude/Codex/OMP's
/// verbatim-workspace-path contract; falls back to the original path if the
/// process's current directory is unavailable.
fn absolutize(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    }
}

/// Determine the [`ActivityStatus`] for a parsed session given optional
/// positive-evidence Session Control association. Active is reported only after
/// a validated stable-ID/path association; otherwise Unknown. Absence of
/// evidence is Unknown, never Inactive.
pub fn activity_status(
    parsed: &ParsedSession,
    control_evidence: Option<&SessionControlEvidence>,
) -> ActivityStatus {
    match control_evidence {
        Some(evidence) if evidence.matches(parsed) => ActivityStatus::Active {
            observed_at: evidence.observed_at,
        },
        _ => ActivityStatus::Unknown,
    }
}

/// Positive-evidence association between a live `session-control` entry and a
/// parsed session. A match requires the stable ID to agree with the control
/// entry and the control entry's transcript path to resolve to the parsed
/// session's transcript locator.
#[derive(Clone, Debug)]
pub struct SessionControlEvidence {
    /// Stable session ID recorded in the control entry.
    pub session_id: String,
    /// Transcript path recorded in the control entry.
    pub transcript_path: PathBuf,
    /// When the association was observed.
    pub observed_at: SystemTime,
}

impl SessionControlEvidence {
    pub(super) fn matches(&self, parsed: &ParsedSession) -> bool {
        if self.session_id != parsed.id {
            return false;
        }
        // The control entry's transcript path must resolve to the same file.
        let self_canon = self.transcript_path.canonicalize().ok();
        let parsed_canon = parsed.transcript_path.canonicalize().ok();
        match (self_canon, parsed_canon) {
            (Some(a), Some(b)) => a == b,
            // Fall back to lexical equality if canonicalization fails.
            _ => self.transcript_path == parsed.transcript_path,
        }
    }
}

/// Compute risk status for a parsed Pi session, including the broad-workspace
/// check against `$HOME`/`/`.
pub fn risk_status(parsed: &ParsedSession, home: Option<&Path>) -> RiskStatus {
    let evidence = match &parsed.workspace {
        Some(workspace) => WorkspaceEvidence::Recorded {
            workspace: workspace.clone(),
            historical_git_identity: None,
        },
        None => return RiskStatus::Normal,
    };
    crate::scope::broad_workspace_risk(&evidence, home)
}

/// Whether a read result indicates a file that was being actively written
/// (incomplete tail). Useful for tests and diagnostics; does not affect
/// discovery correctness since we retain valid records regardless.
pub fn was_live_growing(result: &ReadResult) -> bool {
    matches!(result.outcome, FileOutcome::IncompleteTail)
}

// ---------------------------------------------------------------------------
// Test-exposed wrappers. These are `#[doc(hidden)]` public functions so the
// integration test module (a sibling file) can reach private helpers for
// focused assertions. They are not part of the public API.
// ---------------------------------------------------------------------------

#[doc(hidden)]
pub fn extract_session_pub(
    path: &Path,
    result: &ReadResult,
    file_mtime: Option<SystemTime>,
) -> Option<ParsedSession> {
    extract_session(path, result, file_mtime)
}

#[doc(hidden)]
pub fn settings_dir_pub(settings: &Value) -> Option<PathBuf> {
    settings_dir(settings)
}

#[cfg(test)]
#[path = "tests/resume.rs"]
mod tests;
