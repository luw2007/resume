use std::path::{Path, PathBuf};

/// The environment variable naming the effective Codex data root.
pub const ENV_CODEX_HOME: &str = "CODEX_HOME";

/// Subdirectory holding active (live) rollouts.
const SESSIONS_SUBDIR: &str = "sessions";

/// Subdirectory holding archived rollouts.
const ARCHIVED_SUBDIR: &str = "archived_sessions";

/// Filename prefix for rollout JSONL files.
const ROLLOUT_PREFIX: &str = "rollout-";

/// Filename suffix for rollout JSONL files.
const ROLLOUT_SUFFIX: &str = ".jsonl";

/// Resolve the effective Codex data root.
///
/// Precedence: `$CODEX_HOME` if set and non-empty, otherwise `~/.codex`.
/// The returned path is not canonicalized here (the root may not exist yet);
/// callers canonicalize when building identity.
pub fn effective_root() -> Option<PathBuf> {
    match std::env::var_os(ENV_CODEX_HOME) {
        Some(value) if !value.is_empty() => Some(PathBuf::from(value)),
        _ => dirs_home().map(|home| home.join(".codex")),
    }
}

/// Resolve `$HOME` without depending on a `dirs` crate.
pub(super) fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from).filter(|home| {
        // A truly empty HOME is not a usable home.
        !home.as_os_str().is_empty()
    })
}

/// Candidate rollout roots to scan: active then archived, beneath the
/// effective root. Returns roots that exist; a missing root is not an error
/// (Codex may not have created the archived directory yet).
pub fn rollout_roots(effective_root: &Path) -> Vec<RolloutRoot> {
    let mut roots = Vec::new();
    let active = effective_root.join(SESSIONS_SUBDIR);
    if active.is_dir() {
        roots.push(RolloutRoot {
            path: active,
            kind: RolloutKind::Active,
        });
    }
    let archived = effective_root.join(ARCHIVED_SUBDIR);
    if archived.is_dir() {
        roots.push(RolloutRoot {
            path: archived,
            kind: RolloutKind::Archived,
        });
    }
    roots
}

/// A discovered rollout scan root.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RolloutRoot {
    /// The `sessions` or `archived_sessions` directory (dated subdirs live
    /// beneath it).
    pub path: PathBuf,
    /// Whether this root holds active or archived rollouts.
    pub kind: RolloutKind,
}

/// Kind of rollout root.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RolloutKind {
    /// `sessions/` — live rollouts.
    Active,
    /// `archived_sessions/` — archived rollouts.
    Archived,
}

/// True if the filename looks like a rollout JSONL file (`rollout-*.jsonl`).
pub(crate) fn is_rollout_filename(name: Option<&std::ffi::OsStr>) -> bool {
    let Some(name) = name.and_then(|name| name.to_str()) else {
        return false;
    };
    name.starts_with(ROLLOUT_PREFIX) && name.ends_with(ROLLOUT_SUFFIX)
}
