//! Positive-evidence OMP activity correlation.

use std::{
    collections::HashMap,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use super::{AGENT, ParsedSession, roots::EffectiveRoots};
use crate::{
    proc::ProcessTable,
    session::{ActivityStatus, Diagnostic},
};

pub const BREADCRUMB_FRESHNESS: Duration = Duration::from_secs(12 * 60 * 60);

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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Breadcrumb {
    pub tty: OsString,
    pub session_path: PathBuf,
    pub recorded_at: Option<SystemTime>,
}

pub trait BreadcrumbSource {
    fn breadcrumbs(&self) -> Vec<Breadcrumb>;
}

#[derive(Clone, Debug)]
pub struct OmpBreadcrumbs {
    directory: PathBuf,
}
impl OmpBreadcrumbs {
    pub fn new(roots: &EffectiveRoots) -> Self {
        let overridden = std::env::var_os(super::ENV_AGENT_DIR).is_some();
        let xdg = std::env::var_os("XDG_STATE_HOME").map(PathBuf::from);
        Self {
            directory: breadcrumb_directory(roots, overridden, xdg.as_deref()),
        }
    }
    #[cfg(test)]
    pub(super) fn from_directory(directory: PathBuf) -> Self {
        Self { directory }
    }
}

pub(super) fn breadcrumb_directory(
    roots: &EffectiveRoots,
    overridden: bool,
    xdg: Option<&Path>,
) -> PathBuf {
    let profile = (!overridden)
        .then_some(xdg)
        .flatten()
        .map(|home| home.join(AGENT))
        .map(|root| match &roots.profile {
            super::ProfileSelection::Default => root,
            super::ProfileSelection::Named(name) => root.join("profiles").join(name),
        })
        .filter(|root| root.is_dir());
    profile
        .unwrap_or_else(|| roots.agent_root.clone())
        .join("terminal-sessions")
}

impl BreadcrumbSource for OmpBreadcrumbs {
    fn breadcrumbs(&self) -> Vec<Breadcrumb> {
        let Ok(entries) = fs::read_dir(&self.directory) else {
            return Vec::new();
        };
        entries
            .flatten()
            .filter_map(|entry| {
                let tty = entry.file_name();
                let raw = fs::read_to_string(entry.path()).ok()?;
                let mut lines = raw.lines();
                lines.next()?;
                let session = lines.next()?.trim();
                if session.is_empty() {
                    return None;
                }
                let recorded_at = entry.metadata().ok()?.modified().ok();
                Some(Breadcrumb {
                    tty,
                    session_path: session.into(),
                    recorded_at,
                })
            })
            .collect()
    }
}

#[derive(Clone, Debug, Default)]
pub struct ActivityEvidenceMap {
    canonical: HashMap<PathBuf, ActivityEvidence>,
    lexical: HashMap<PathBuf, ActivityEvidence>,
}
impl ActivityEvidenceMap {
    pub fn for_transcript(&self, path: &Path) -> Option<&ActivityEvidence> {
        path.canonicalize()
            .ok()
            .and_then(|p| self.canonical.get(&p))
            .or_else(|| self.lexical.get(path))
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

pub fn correlate_live(
    procs: &ProcessTable,
    roots: &[EffectiveRoots],
) -> (ActivityEvidenceMap, Vec<Diagnostic>) {
    let now = procs.observed_at().unwrap_or_else(SystemTime::now);
    let mut all = ActivityEvidenceMap::default();
    let mut diagnostics = Vec::new();
    for root in roots {
        let (map, mut found) = correlate_live_with(procs, &OmpBreadcrumbs::new(root), now);
        all.canonical.extend(map.canonical);
        all.lexical.extend(map.lexical);
        diagnostics.append(&mut found);
    }
    (all, diagnostics)
}

pub fn correlate_live_with(
    procs: &ProcessTable,
    source: &dyn BreadcrumbSource,
    now: SystemTime,
) -> (ActivityEvidenceMap, Vec<Diagnostic>) {
    let mut result = ActivityEvidenceMap::default();
    let mut diagnostics = Vec::new();
    for breadcrumb in source.breadcrumbs() {
        let Some(process) = procs.live_on_tty(AGENT, &breadcrumb.tty) else {
            continue;
        };
        let Some(recorded_at) = breadcrumb.recorded_at else {
            continue;
        };
        let fresh = match process.started_at {
            Some(started_at) => recorded_at >= started_at,
            None => {
                diagnostics.push(Diagnostic {
                    category: crate::errors::category::OMP_BREADCRUMB_START_TIME_UNAVAILABLE,
                    count: 1,
                    verbose_path: Some(breadcrumb.session_path.clone()),
                    verbose_chain: None,
                });
                now.duration_since(recorded_at)
                    .is_ok_and(|age| age <= BREADCRUMB_FRESHNESS)
            }
        };
        if !fresh || !breadcrumb.session_path.is_file() {
            continue;
        }
        let path = breadcrumb.session_path;
        result.insert(
            path.clone(),
            ActivityEvidence {
                live_process: true,
                tty: process.tty.clone(),
                breadcrumb_alive: true,
                breadcrumb_session_path: path,
                observed_at: now,
            },
        );
    }
    (result, diagnostics)
}
