//! Discovery of OMP child execution records.
//!
//! OMP child sessions are stored as `.jsonl` files under a directory named
//! after the parent session file stem: if the parent is `abc.jsonl`, its
//! children live under `abc/*.jsonl`. These are NOT independent Sessions and
//! never surface as resumable.

use super::format::ImportBadge;
use crate::preview::jsonl::{self, Bounds};
use crate::session::Diagnostic;
use serde_json::Value;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// A discovered OMP child execution record. Never becomes a [`crate::session::Session`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChildExecution {
    /// Parent session locator (canonical path of the parent `.jsonl` file).
    pub parent_locator: PathBuf,
    /// The child's header `id`, if a valid v3 session header exists.
    pub child_id: Option<String>,
    /// Filename (e.g. `worker.jsonl`) or agent name from the child's header.
    pub name: Option<String>,
    /// Working directory recorded in the child's header.
    pub cwd: Option<PathBuf>,
    /// Whether the child transcript had any recognizable records.
    pub has_activity: bool,
    /// Canonical path to the child transcript file.
    pub locator: PathBuf,
    /// Import badge preserved from parent, if the child itself carries one.
    pub import: Option<ImportBadge>,
}

/// Result of child-execution discovery under an OMP session root.
#[derive(Clone, Debug, Default)]
pub struct ChildDiscovery {
    pub children: Vec<ChildExecution>,
    pub diagnostics: Vec<Diagnostic>,
}

/// Discover child executions for the given parent transcripts only. Each
/// parent `<dir>/<stem>.jsonl` owns the sibling directory `<dir>/<stem>/`;
/// nothing else under `session_root` is visited. Parents are deduplicated and
/// processed in sorted order, and children within a directory are sorted, so
/// output is deterministic. Reads stay confined to `session_root`.
pub fn discover_children_for_parents(session_root: &Path, parents: &[PathBuf]) -> ChildDiscovery {
    let mut result = ChildDiscovery::default();
    let confined_root = session_root
        .canonicalize()
        .unwrap_or_else(|_| session_root.to_path_buf());
    let parents: BTreeSet<&PathBuf> = parents.iter().collect();
    for parent_path in parents {
        let (Some(dir), Some(stem)) = (parent_path.parent(), parent_path.file_stem()) else {
            continue;
        };
        let Ok(canonical_dir) = dir.canonicalize() else {
            continue;
        };
        let child_dir = dir.join(stem);
        // Both the parent's directory and the child directory must resolve
        // inside the root, and the child directory must be exactly the
        // sibling `<stem>/` (not a symlink redirected elsewhere).
        let Ok(canonical_child) = child_dir.canonicalize() else {
            continue;
        };
        if !canonical_dir.starts_with(&confined_root)
            || canonical_child != canonical_dir.join(stem)
            || !canonical_child.is_dir()
        {
            continue;
        }
        parse_child_dir(&child_dir, parent_path, &confined_root, &mut result);
    }
    result
}

/// Parse all `.jsonl` files in a child directory.
fn parse_child_dir(
    child_dir: &Path,
    parent_path: &Path,
    confined_root: &Path,
    result: &mut ChildDiscovery,
) {
    let entries = match std::fs::read_dir(child_dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        if !entry
            .file_type()
            .map(|t| t.is_file() || t.is_symlink())
            .unwrap_or(false)
        {
            continue;
        }
        match parse_child_file(&path, parent_path, confined_root) {
            Ok(child) => result.children.push(child),
            Err(diag) => result.diagnostics.push(diag),
        }
    }
}

/// Parse a single child `.jsonl` transcript.
fn parse_child_file(
    path: &Path,
    parent_path: &Path,
    confined_root: &Path,
) -> Result<ChildExecution, Diagnostic> {
    let read = jsonl::read_file_confined(path, confined_root, &Bounds::default()).map_err(|e| {
        Diagnostic {
            category: "omp_child_io",
            count: 1,
            verbose_path: Some(path.to_path_buf()),
            verbose_chain: Some(e.to_string()),
        }
    })?;

    let mut child_id: Option<String> = None;
    let mut cwd: Option<PathBuf> = None;
    let mut name: Option<String> = None;
    let mut has_activity = false;
    let mut import: Option<ImportBadge> = None;

    for record in &read.records {
        let rec_type = record.get("type").and_then(Value::as_str);

        if !has_activity && rec_type.is_some() {
            has_activity = true;
        }

        // v3 session header
        if rec_type == Some("session") {
            if child_id.is_none() {
                child_id = record
                    .get("id")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(String::from);
            }
            if cwd.is_none() {
                cwd = record
                    .get("cwd")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(PathBuf::from);
            }
            if name.is_none() {
                name = record
                    .get("title")
                    .and_then(Value::as_str)
                    .filter(|s| !s.trim().is_empty())
                    .map(|s| s.trim().to_string());
            }
        }

        // title record
        if rec_type == Some("title") && name.is_none() {
            name = record
                .get("title")
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty())
                .map(|s| s.trim().to_string());
        }

        // foreign import badge on the child itself
        if rec_type == Some("session")
            && import.is_none()
            && let Some(fi) = record.get("foreign_session_import")
        {
            import = super::format::parse_import_pub(fi);
        }
    }

    // Fallback name from filename
    if name.is_none() {
        name = path.file_stem().and_then(|s| s.to_str()).map(String::from);
    }

    Ok(ChildExecution {
        parent_locator: parent_path.to_path_buf(),
        child_id,
        name,
        cwd,
        has_activity,
        locator: path.to_path_buf(),
        import,
    })
}

#[cfg(test)]
#[path = "tests/children.rs"]
mod tests;
