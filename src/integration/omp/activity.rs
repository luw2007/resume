//! Positive-evidence OMP activity correlation.

use std::{
    collections::HashMap,
    ffi::{OsStr, OsString},
    fs,
    path::{Path, PathBuf},
    time::SystemTime,
};

use crate::{proc::ProcessTable, session::ActivityStatus};

use super::{AGENT, ParsedSession, roots::EffectiveRoots};

/// Determine activity from already-correlated evidence.
pub fn activity_status(
    parsed: &ParsedSession,
    evidence: Option<&ActivityEvidence>,
) -> ActivityStatus {
    match evidence {
        Some(evidence) if evidence.matches(parsed) => ActivityStatus::Active {
            observed_at: evidence.observed_at,
        },
        _ => ActivityStatus::Unknown,
    }
}

/// Positive correlation of a live process, its TTY, and a transcript breadcrumb.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActivityEvidence {
    pub live_process: bool,
    pub tty: Option<OsString>,
    pub breadcrumb_alive: bool,
    pub breadcrumb_session_path: PathBuf,
    pub observed_at: SystemTime,
}

impl ActivityEvidence {
    fn matches(&self, parsed: &ParsedSession) -> bool {
        if !self.live_process || self.tty.is_none() || !self.breadcrumb_alive {
            return false;
        }
        match (
            self.breadcrumb_session_path.canonicalize().ok(),
            parsed.transcript_path.canonicalize().ok(),
        ) {
            (Some(a), Some(b)) => a == b,
            _ => self.breadcrumb_session_path == parsed.transcript_path,
        }
    }
}

/// Read-only mapping from a terminal name to its last OMP transcript.
pub trait BreadcrumbSource {
    fn session_for_tty(&self, tty: &OsStr) -> Option<PathBuf>;

    fn is_alive(&self, tty: &OsStr) -> bool {
        self.session_for_tty(tty).is_some()
    }
}

/// OMP's per-profile terminal breadcrumb directory.
///
/// OMP 17.2.12 stores one bare-text file per terminal at
/// `<agent-state-root>/terminal-sessions/<tty>`. Line one is cwd, line two is
/// the absolute session JSONL path, and optional line three is `fresh`.
#[derive(Clone, Debug)]
pub struct OmpBreadcrumbs {
    directory: PathBuf,
}

impl OmpBreadcrumbs {
    pub fn new(roots: &EffectiveRoots) -> Self {
        let agent_dir_overridden = std::env::var_os(super::ENV_AGENT_DIR).is_some();
        let xdg_state_home = std::env::var_os("XDG_STATE_HOME").map(PathBuf::from);
        Self {
            directory: breadcrumb_directory(roots, agent_dir_overridden, xdg_state_home.as_deref()),
        }
    }

    #[cfg(test)]
    pub(super) fn from_directory(directory: PathBuf) -> Self {
        Self { directory }
    }
}

pub(super) fn breadcrumb_directory(
    roots: &EffectiveRoots,
    agent_dir_overridden: bool,
    xdg_state_home: Option<&Path>,
) -> PathBuf {
    let xdg_profile_root = (!agent_dir_overridden)
        .then_some(xdg_state_home)
        .flatten()
        .map(|home| home.join(AGENT))
        .map(|root| match &roots.profile {
            super::ProfileSelection::Default => root,
            super::ProfileSelection::Named(name) => root.join("profiles").join(name),
        })
        .filter(|root| root.is_dir());
    xdg_profile_root
        .unwrap_or_else(|| roots.agent_root.clone())
        .join("terminal-sessions")
}

impl BreadcrumbSource for OmpBreadcrumbs {
    fn session_for_tty(&self, tty: &OsStr) -> Option<PathBuf> {
        // A terminal identifier is a filename, never a path supplied by the user.
        if Path::new(tty).file_name() != Some(tty) {
            return None;
        }
        let raw = fs::read_to_string(self.directory.join(tty)).ok()?;
        let mut lines = raw.lines();
        let _cwd = lines.next()?;
        let session = lines.next()?.trim();
        if session.is_empty() {
            None
        } else {
            Some(PathBuf::from(session))
        }
    }
}

/// O(1) transcript-to-evidence lookup.
#[derive(Clone, Debug, Default)]
pub struct ActivityEvidenceMap {
    canonical: HashMap<PathBuf, ActivityEvidence>,
    lexical: HashMap<PathBuf, ActivityEvidence>,
}

impl ActivityEvidenceMap {
    pub fn for_transcript(&self, transcript_path: &Path) -> Option<&ActivityEvidence> {
        transcript_path
            .canonicalize()
            .ok()
            .and_then(|path| self.canonical.get(&path))
            .or_else(|| self.lexical.get(transcript_path))
    }

    pub fn is_empty(&self) -> bool {
        self.canonical.is_empty() && self.lexical.is_empty()
    }

    fn insert(&mut self, path: PathBuf, evidence: ActivityEvidence) {
        if let Ok(canonical) = path.canonicalize() {
            self.canonical.insert(canonical, evidence.clone());
        }
        self.lexical.insert(path, evidence);
    }
}

/// Correlate live OMP terminals against all resolved profile breadcrumb stores.
pub fn correlate_live(procs: &ProcessTable, roots: &[EffectiveRoots]) -> ActivityEvidenceMap {
    let observed_at = procs.observed_at().unwrap_or_else(SystemTime::now);
    let mut result = ActivityEvidenceMap::default();
    for root in roots {
        let correlated = correlate_live_with(procs, &OmpBreadcrumbs::new(root), observed_at);
        result.canonical.extend(correlated.canonical);
        result.lexical.extend(correlated.lexical);
    }
    result
}

/// Unit-testable correlation with an explicit breadcrumb source.
pub fn correlate_live_with<B: BreadcrumbSource>(
    procs: &ProcessTable,
    breadcrumbs: &B,
    observed_at: SystemTime,
) -> ActivityEvidenceMap {
    let mut result = ActivityEvidenceMap::default();
    for tty in procs.ttys_for_command(AGENT) {
        let Some(path) = breadcrumbs.session_for_tty(&tty) else {
            continue;
        };
        if !breadcrumbs.is_alive(&tty) || !path.is_file() {
            continue;
        }
        result.insert(
            path.clone(),
            ActivityEvidence {
                live_process: true,
                tty: Some(tty),
                breadcrumb_alive: true,
                breadcrumb_session_path: path,
                observed_at,
            },
        );
    }
    result
}
