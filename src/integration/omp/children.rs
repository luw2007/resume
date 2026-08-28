//! Discovery of OMP child execution records.
//!
//! OMP child sessions are stored as `.jsonl` files under a directory named
//! after the parent session file stem: if the parent is `abc.jsonl`, its
//! children live under `abc/*.jsonl`. These are NOT independent Sessions and
//! never surface as resumable.

use crate::preview::jsonl::{self, Bounds};
use crate::session::Diagnostic;
use super::format::ImportBadge;
use serde_json::Value;
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

/// Discover child executions under the given session root. For each `.jsonl`
/// file, checks for a sibling directory named `<stem>/` containing child
/// transcripts.
pub fn discover_children(session_root: &Path) -> ChildDiscovery {
    let mut result = ChildDiscovery::default();
    let confined_root = session_root
        .canonicalize()
        .unwrap_or_else(|_| session_root.to_path_buf());
    discover_children_recursive(session_root, &confined_root, &mut result);
    result
}

fn discover_children_recursive(dir: &Path, confined_root: &Path, result: &mut ChildDiscovery) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };

    let mut parent_files: Vec<PathBuf> = Vec::new();
    let mut child_dirs: Vec<PathBuf> = Vec::new();

    for entry in entries.flatten() {
        let path = entry.path();
        let ft = match entry.file_type() {
            Ok(ft) => ft,
            Err(_) => continue,
        };
        if ft.is_dir() {
            child_dirs.push(path);
        } else if (ft.is_file() || ft.is_symlink())
            && path.extension().and_then(|e| e.to_str()) == Some("jsonl")
        {
            parent_files.push(path);
        }
    }

    // For each parent .jsonl, check if there's a matching stem directory
    for parent_path in &parent_files {
        let stem = match parent_path.file_stem().and_then(|s| s.to_str()) {
            Some(s) => s.to_string(),
            None => continue,
        };
        let child_dir = dir.join(&stem);
        if !child_dir.is_dir() {
            continue;
        }
        // Found a child directory — parse its contents
        parse_child_dir(&child_dir, parent_path, confined_root, result);
    }

    // Recurse into grouped workspace directories (those starting with '-')
    // to find parent files deeper in the tree, but skip child directories
    // we already processed above.
    for d in &child_dirs {
        let name = d.file_name().and_then(|n| n.to_str()).unwrap_or("");
        // Only recurse into workspace-encoded dirs (start with '-') or
        // structural dirs, not into child dirs we already handled.
        let is_child_of_parent = parent_files.iter().any(|p| {
            p.file_stem().and_then(|s| s.to_str()) == Some(name)
        });
        if !is_child_of_parent {
            discover_children_recursive(d, confined_root, result);
        }
    }
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
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        if !entry.file_type().map(|t| t.is_file() || t.is_symlink()).unwrap_or(false) {
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
        if rec_type == Some("title") {
            if name.is_none() {
                name = record
                    .get("title")
                    .or_else(|| record.get("text"))
                    .and_then(Value::as_str)
                    .filter(|s| !s.trim().is_empty())
                    .map(|s| s.trim().to_string());
            }
        }

        // foreign import badge on the child itself
        if rec_type == Some("session") && import.is_none() {
            if let Some(fi) = record.get("foreign_session_import") {
                import = super::format::parse_import_pub(fi);
            }
        }
    }

    // Fallback name from filename
    if name.is_none() {
        name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .map(String::from);
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
