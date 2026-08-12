use super::{format::parse_candidate, roots::ClaudeRoot};
use crate::session::{Diagnostic, IntegrationError, Session};
use std::{
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
};
const PROJECTS_DIR: &str = "projects";
/// A discovered Claude transcript candidate, before parsing and Scope filtering.
pub(super) struct Candidate {
    /// Canonical real path of the `.jsonl` transcript.
    pub(super) path: PathBuf,
    /// The stem (filename without extension), expected to be a UUID.
    pub(super) stem: OsString,
}

/// Result of discovering Claude Sessions under a resolved root, before Scope
/// filtering. Each [`Session`] has its recorded Workspace populated from event
/// `cwd`; Scope membership is applied by the caller.
#[derive(Clone, Debug)]
pub struct Discovery {
    pub sessions: Vec<Session>,
    pub diagnostics: Vec<Diagnostic>,
    /// Number of workspace-key directories pruned by the directory-name
    /// filter without reading any file inside them.
    pub pruned_dirs: usize,
}

impl Discovery {
    fn new() -> Self {
        Self {
            sessions: Vec::new(),
            diagnostics: Vec::new(),
            pruned_dirs: 0,
        }
    }
}

/// Discover Claude Sessions under a resolved root, without directory
/// pruning (scans every workspace-key directory).
pub fn discover(root: &ClaudeRoot) -> Result<Discovery, IntegrationError> {
    discover_with_dir_filter(root, |_| true)
}

/// Discover Claude Sessions under a resolved root, pruning workspace-key
/// directories the filter rejects.
///
/// `dir_filter` receives each dash-prefixed workspace-key directory name
/// (Claude encodes the Workspace path into the name by mapping every
/// non-alphanumeric character to '-'); returning `false` skips the whole
/// directory without reading any file inside it (counted in
/// [`Discovery::pruned_dirs`]). Follows the same convention as Codex's
/// `discover_with_filter`: typically a Scope membership prefilter
/// (`Scope::may_contain_session_dir`). Non-dash-prefixed directory names
/// carry no encoding and are never offered to the filter.
///
/// Scans only valid top-level Session transcripts (direct `.jsonl` children of
/// each workspace-key directory), excluding nested subagent artifacts. Each
/// retained Session carries the recorded `cwd` as its Workspace evidence so
/// callers can apply authoritative Scope membership. Read-only: no file is
/// opened for write, no directory entry or mtime is changed, and the Claude
/// CLI is never invoked.
pub fn discover_with_dir_filter<F>(
    root: &ClaudeRoot,
    dir_filter: F,
) -> Result<Discovery, IntegrationError>
where
    F: Fn(&str) -> bool,
{
    let projects = root.effective_root.join(PROJECTS_DIR);
    let projects_real = match projects.canonicalize() {
        Ok(path) => path,
        Err(_) => {
            // No `projects` directory: nothing to discover, not an error.
            return Ok(Discovery::new());
        }
    };

    let mut discovery = Discovery::new();
    let candidates = collect_candidates(&projects_real, &dir_filter, &mut discovery);

    for candidate in candidates {
        match parse_candidate(&candidate, root) {
            Ok((Some(session), nonfatal)) => {
                discovery.sessions.push(session);
                discovery.diagnostics.extend(nonfatal);
            }
            Ok((None, nonfatal)) => {
                discovery.diagnostics.extend(nonfatal);
            }
            Err(diagnostic) => discovery.diagnostics.push(diagnostic),
        }
    }

    Ok(discovery)
}

/// Collect top-level transcript candidates, excluding nested subagent dirs.
///
/// Layout: `<projects>/<workspace-key>/<uuid>.jsonl`. A workspace-key directory
/// may itself contain a `subagents/` directory with its own transcripts; those
/// nested files are never surfaced as independent top-level Sessions because we
/// only enumerate the **direct** `.jsonl` children of each workspace-key
/// directory.
fn collect_candidates<F>(
    projects: &Path,
    dir_filter: &F,
    discovery: &mut Discovery,
) -> Vec<Candidate>
where
    F: Fn(&str) -> bool,
{
    let mut candidates = Vec::new();
    let workspace_dirs = match fs::read_dir(projects) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return candidates,
        Err(error) => {
            discovery.diagnostics.push(Diagnostic {
                category: "claude_root_unavailable",
                count: 1,
                verbose_path: Some(projects.to_path_buf()),
                verbose_chain: Some(error.to_string()),
            });
            return candidates;
        }
    };

    for entry in workspace_dirs.flatten() {
        if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        if let Some(name) = entry.file_name().to_str()
            && name.starts_with('-')
            && !dir_filter(name)
        {
            discovery.pruned_dirs += 1;
            continue;
        }
        let workspace_key_dir = entry.path();

        // Direct `.jsonl` children of the workspace-key directory are
        // top-level transcripts. Nested directories (e.g. `subagents/`) are
        // not descended into, so their artifacts are never independent
        // Sessions.
        let top_level = match fs::read_dir(&workspace_key_dir) {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        for transcript in top_level.flatten() {
            let path = transcript.path();
            if !is_transcript(&path, &transcript) {
                continue;
            }
            let stem = match path.file_stem() {
                Some(stem) => stem.to_os_string(),
                None => {
                    discovery.diagnostics.push(diagnostic_count(
                        "claude_skipped",
                        &path,
                        "transcript without filename stem",
                    ));
                    continue;
                }
            };
            candidates.push(Candidate { path, stem });
        }
    }

    candidates
}

/// Whether a directory entry is a `.jsonl` file (a regular file or symlink,
/// not a directory).
fn is_transcript(path: &Path, entry: &fs::DirEntry) -> bool {
    if !path_is_extension(path, "jsonl") {
        return false;
    }
    match entry.file_type() {
        Ok(file_type) => file_type.is_file() || file_type.is_symlink(),
        Err(_) => false,
    }
}

/// Case-insensitive extension check that does not force UTF-8.
fn path_is_extension(path: &Path, expected_lower: &str) -> bool {
    let Some(ext) = path.extension() else {
        return false;
    };
    let Some(ext_str) = ext.to_str() else {
        return false;
    };
    ext_str.eq_ignore_ascii_case(expected_lower)
}

#[cfg(test)]
#[path = "tests/discover.rs"]
mod tests;
// --- diagnostic helpers ---

pub(super) fn diagnostic_count(category: &'static str, path: &Path, _note: &str) -> Diagnostic {
    Diagnostic {
        category,
        count: 1,
        verbose_path: Some(path.to_path_buf()),
        verbose_chain: None,
    }
}

pub(super) fn diagnostic_chain(category: &'static str, path: &Path, chain: &str) -> Diagnostic {
    Diagnostic {
        category,
        count: 1,
        verbose_path: Some(path.to_path_buf()),
        verbose_chain: Some(chain.to_string()),
    }
}
