//! Discovery of Claude subagent execution records.
//!
//! Claude Code stores subagent transcripts in two layouts:
//!
//! * nested (current native): `projects/<workspace-key>/<parent-uuid>/subagents/agent-*.jsonl`.
//!   The directory name is the parent session UUID; it is the authoritative
//!   parent association ([`EvidenceSource::NativeLayout`]).
//! * flat: `projects/<workspace-key>/subagents/*.jsonl`. The parent comes from
//!   explicit transcript metadata (`parentSessionId` / `parent_session_id`) or,
//!   failing that, the sole top-level sibling transcript
//!   ([`EvidenceSource::NativeTranscript`]).
//!
//! These are NOT independent Sessions and never surface as resumable. They are
//! adapter-owned execution records tied to their parent session.

use crate::preview::jsonl::{self, Bounds};
use crate::relation::EvidenceSource;
use crate::session::Diagnostic;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// A discovered subagent execution record. Never becomes a [`crate::session::Session`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChildExecution {
    /// Parent session UUID: the nested layout's parent directory name, or the
    /// flat layout's explicit `parentSessionId` / sole-sibling filename stem.
    pub parent_id: String,
    /// Evidence class that produced `parent_id`.
    pub source: EvidenceSource,
    /// The subagent's own agent ID, if embedded in the transcript.
    pub agent_id: Option<String>,
    /// Native locator: path to the subagent transcript file.
    pub locator: PathBuf,
    /// Agent or display name recorded in the transcript.
    pub name: Option<String>,
    /// Working directory recorded in the transcript.
    pub cwd: Option<PathBuf>,
    /// Whether the transcript contained recognizable Claude structural fields.
    pub has_activity: bool,
}

/// Result of discovering subagent executions.
#[derive(Clone, Debug, Default)]
pub struct ChildDiscovery {
    pub children: Vec<ChildExecution>,
    pub diagnostics: Vec<Diagnostic>,
}

/// Discover subagent executions for the given parent transcripts
/// (`<projects>/<workspace-key>/<uuid>.jsonl`) only. For each parent the
/// nested `<workspace-key>/<uuid>/subagents/` directory is read; each distinct
/// workspace-key directory additionally has its flat `subagents/` directory
/// read. Every directory is canonicalized and must resolve to its expected
/// location inside `projects_dir` before it is listed. Parents and directories
/// are deduplicated and visited in sorted order so output is deterministic.
/// The flat sole-top-level-transcript fallback still counts every top-level
/// transcript in the directory, so out-of-scope siblings keep a parentless
/// child ambiguous.
pub fn discover_children_for_parents(projects_dir: &Path, parents: &[PathBuf]) -> ChildDiscovery {
    let mut result = ChildDiscovery::default();
    let confined_root = projects_dir
        .canonicalize()
        .unwrap_or_else(|_| projects_dir.to_path_buf());

    let mut by_workspace: BTreeMap<&Path, BTreeSet<String>> = BTreeMap::new();
    for parent in parents {
        let (Some(dir), Some(stem)) =
            (parent.parent(), parent.file_stem().and_then(|s| s.to_str()))
        else {
            continue;
        };
        by_workspace
            .entry(dir)
            .or_default()
            .insert(stem.to_string());
    }

    for (workspace_key_dir, stems) in by_workspace {
        let Some(canonical_workspace) = confined_dir(workspace_key_dir, &confined_root, None)
        else {
            continue;
        };
        // Nested layout: directory name is the authoritative parent.
        for stem in &stems {
            let subagents_dir = workspace_key_dir.join(stem).join("subagents");
            let expected = canonical_workspace.join(stem).join("subagents");
            if confined_dir(&subagents_dir, &confined_root, Some(&expected)).is_none() {
                continue;
            }
            scan_dir(
                &subagents_dir,
                &confined_root,
                &Layout::Nested { parent_uuid: stem },
                &mut result,
            );
        }
        // Flat layout.
        let subagents_dir = workspace_key_dir.join("subagents");
        let expected = canonical_workspace.join("subagents");
        if confined_dir(&subagents_dir, &confined_root, Some(&expected)).is_some() {
            let parent_uuids = collect_parent_uuids(workspace_key_dir);
            scan_dir(
                &subagents_dir,
                &confined_root,
                &Layout::Flat {
                    parent_uuids: &parent_uuids,
                },
                &mut result,
            );
        }
    }
    result
}

/// Canonicalize `dir`; return it only when it is a directory inside `root`
/// and (when given) equal to `expected`. Rejects symlink escapes and
/// symlinks that redirect to a different in-root directory.
fn confined_dir(dir: &Path, root: &Path, expected: Option<&Path>) -> Option<PathBuf> {
    let canonical = dir.canonicalize().ok()?;
    if !canonical.starts_with(root) || !canonical.is_dir() {
        return None;
    }
    if expected.is_some_and(|e| e != canonical) {
        return None;
    }
    Some(canonical)
}

enum Layout<'a> {
    Nested { parent_uuid: &'a str },
    Flat { parent_uuids: &'a [String] },
}

/// Scan `*.jsonl` in an already-confined subagents directory.
fn scan_dir(
    subagents_dir: &Path,
    confined_root: &Path,
    layout: &Layout<'_>,
    result: &mut ChildDiscovery,
) {
    let Ok(entries) = std::fs::read_dir(subagents_dir) else {
        return;
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
        match parse_child_transcript(&path, confined_root, layout) {
            Ok(child) => result.children.push(child),
            Err(diag) => result.diagnostics.push(diag),
        }
    }
}

/// Collect UUID stems of top-level `.jsonl` files in the workspace-key dir.
/// The caller has already confined `workspace_key_dir`.
fn collect_parent_uuids(workspace_key_dir: &Path) -> Vec<String> {
    let mut uuids = Vec::new();
    let Ok(entries) = std::fs::read_dir(workspace_key_dir) else {
        return uuids;
    };
    for entry in entries.flatten() {
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
        if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
            uuids.push(stem.to_string());
        }
    }
    uuids
}

/// Parse a subagent transcript into a [`ChildExecution`].
fn parse_child_transcript(
    path: &Path,
    confined_root: &Path,
    layout: &Layout<'_>,
) -> Result<ChildExecution, Diagnostic> {
    let read = jsonl::read_file_confined(path, confined_root, &Bounds::default()).map_err(|e| {
        Diagnostic {
            category: "claude_child_io",
            count: 1,
            verbose_path: Some(path.to_path_buf()),
            verbose_chain: Some(e.to_string()),
        }
    })?;

    let mut explicit_parent: Option<String> = None;
    let mut conflicting_parent = false;
    let mut agent_id: Option<String> = None;
    let mut session_id: Option<String> = None;
    let mut name: Option<String> = None;
    let mut cwd: Option<PathBuf> = None;
    let mut has_activity = false;

    for record in &read.records {
        if !has_activity
            && [
                "type",
                "sessionId",
                "cwd",
                "uuid",
                "parentSessionId",
                "agentId",
            ]
            .iter()
            .any(|k| record.get(*k).is_some())
        {
            has_activity = true;
        }

        // Explicit parent link; differing values across records are a conflict.
        for key in ["parentSessionId", "parent_session_id"] {
            if let Some(value) = nonempty_str(record, key) {
                match &explicit_parent {
                    None => explicit_parent = Some(value),
                    Some(existing) if *existing != value => conflicting_parent = true,
                    Some(_) => {}
                }
            }
        }

        if agent_id.is_none() {
            agent_id = nonempty_str(record, "agentId");
        }
        if session_id.is_none() {
            session_id = nonempty_str(record, "sessionId");
        }
        if name.is_none() {
            name = first_nonempty_str(record, &["agent-name", "agentName", "agent_name"]);
        }
        if cwd.is_none() {
            cwd = nonempty_str(record, "cwd").map(PathBuf::from);
        }
    }

    let ambiguous = |category: &'static str| Diagnostic {
        category,
        count: 1,
        verbose_path: Some(path.to_path_buf()),
        verbose_chain: None,
    };
    if conflicting_parent {
        return Err(ambiguous("claude_subagent_parent_conflict"));
    }

    let (parent_id, source) = match layout {
        Layout::Nested { parent_uuid } => {
            // The directory is authoritative; explicit metadata may only agree.
            if explicit_parent
                .as_deref()
                .is_some_and(|p| p != *parent_uuid)
            {
                return Err(ambiguous("claude_subagent_parent_conflict"));
            }
            ((*parent_uuid).to_string(), EvidenceSource::NativeLayout)
        }
        Layout::Flat { parent_uuids } => match explicit_parent {
            Some(parent) => (parent, EvidenceSource::NativeTranscript),
            None if parent_uuids.len() == 1 => {
                (parent_uuids[0].clone(), EvidenceSource::NativeTranscript)
            }
            None => return Err(ambiguous("claude_subagent_parent_ambiguous")),
        },
    };

    // Native nested transcripts carry the parent's id in `sessionId` and the
    // child's own id in `agentId`; older flat ones carry the child's own id in
    // `sessionId`.
    let agent_id = agent_id.or_else(|| session_id.filter(|s| *s != parent_id));

    Ok(ChildExecution {
        parent_id,
        source,
        agent_id,
        locator: path.to_path_buf(),
        name,
        cwd,
        has_activity,
    })
}

fn nonempty_str(record: &Value, key: &str) -> Option<String> {
    record
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(String::from)
}

fn first_nonempty_str(record: &Value, keys: &[&str]) -> Option<String> {
    for key in keys {
        if let Some(val) = record.get(*key).and_then(Value::as_str) {
            let trimmed = val.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
#[path = "tests/children.rs"]
mod tests;
