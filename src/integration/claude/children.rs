//! Discovery of Claude subagent execution records.
//!
//! Claude Code stores subagent transcripts under `projects/<workspace-key>/subagents/*.jsonl`.
//! These are NOT independent Sessions and never surface as resumable. They are
//! adapter-owned execution records tied to their parent session.

use crate::preview::jsonl::{self, Bounds};
use crate::session::Diagnostic;
use serde_json::Value;
use std::path::{Path, PathBuf};

/// A discovered subagent execution record. Never becomes a [`crate::session::Session`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChildExecution {
    /// Parent session's embedded `sessionId` (when determinable from the
    /// subagent transcript's own `parentSessionId` field), or the parent
    /// filename UUID as fallback locator.
    pub parent_id: String,
    /// The subagent's own session/agent ID, if embedded in the transcript.
    pub agent_id: Option<String>,
    /// Native locator: canonical path to the subagent transcript file.
    pub locator: PathBuf,
    /// Agent or display name recorded in the transcript.
    pub name: Option<String>,
    /// Working directory recorded in the transcript.
    pub cwd: Option<PathBuf>,
    /// Whether the transcript contained recognizable Claude structural fields.
    pub has_activity: bool,
}

/// Result of discovering subagent executions under a workspace-key directory.
#[derive(Clone, Debug, Default)]
pub struct ChildDiscovery {
    pub children: Vec<ChildExecution>,
    pub diagnostics: Vec<Diagnostic>,
}

/// Discover subagent executions for all workspace-key directories under the
/// projects root. Returns children linked to their parent session.
pub fn discover_children(projects_dir: &Path) -> ChildDiscovery {
    let mut result = ChildDiscovery::default();
    let confined_root = projects_dir
        .canonicalize()
        .unwrap_or_else(|_| projects_dir.to_path_buf());
    let entries = match std::fs::read_dir(projects_dir) {
        Ok(e) => e,
        Err(_) => return result,
    };
    for entry in entries.flatten() {
        if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let workspace_key_dir = entry.path();
        discover_subagents_in_workspace(&workspace_key_dir, &confined_root, &mut result);
    }
    result
}

/// Scan `<workspace-key>/subagents/*.jsonl` for child execution records.
fn discover_subagents_in_workspace(
    workspace_key_dir: &Path,
    confined_root: &Path,
    result: &mut ChildDiscovery,
) {
    let subagents_dir = workspace_key_dir.join("subagents");
    let entries = match std::fs::read_dir(&subagents_dir) {
        Ok(e) => e,
        Err(_) => return, // No subagents dir — normal.
    };

    // Collect parent session UUIDs from sibling top-level transcripts for
    // fallback parent linking.
    let parent_uuids = collect_parent_uuids(workspace_key_dir);

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
        match parse_child_transcript(&path, confined_root, &parent_uuids) {
            Ok(child) => result.children.push(child),
            Err(diag) => result.diagnostics.push(diag),
        }
    }
}

/// Collect UUID stems of top-level `.jsonl` files in the workspace-key dir.
fn collect_parent_uuids(workspace_key_dir: &Path) -> Vec<String> {
    let mut uuids = Vec::new();
    let entries = match std::fs::read_dir(workspace_key_dir) {
        Ok(e) => e,
        Err(_) => return uuids,
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
    parent_uuids: &[String],
) -> Result<ChildExecution, Diagnostic> {
    let read = jsonl::read_file_confined(path, confined_root, &Bounds::default()).map_err(|e| {
        Diagnostic {
            category: "claude_child_io",
            count: 1,
            verbose_path: Some(path.to_path_buf()),
            verbose_chain: Some(e.to_string()),
        }
    })?;

    let mut parent_id: Option<String> = None;
    let mut agent_id: Option<String> = None;
    let mut name: Option<String> = None;
    let mut cwd: Option<PathBuf> = None;
    let mut has_activity = false;

    for record in &read.records {
        // Structural detection
        if !has_activity
            && ["type", "sessionId", "cwd", "uuid", "parentSessionId"]
                .iter()
                .any(|k| record.get(*k).is_some())
        {
            has_activity = true;
        }

        // Parent session link
        if parent_id.is_none() {
            parent_id = record
                .get("parentSessionId")
                .or_else(|| record.get("parent_session_id"))
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(String::from);
        }

        // Own agent/session ID
        if agent_id.is_none() {
            agent_id = record
                .get("sessionId")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(String::from);
        }

        // Agent name
        if name.is_none() {
            name = first_nonempty_str(record, &["agent-name", "agentName", "agent_name"]);
        }

        // Working directory
        if cwd.is_none() {
            cwd = record
                .get("cwd")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(PathBuf::from);
        }
    }

    // A child without explicit parent metadata is linkable only when the
    // containing workspace directory has exactly one top-level Session.
    let parent_id =
        parent_id.or_else(|| (parent_uuids.len() == 1).then(|| parent_uuids[0].clone()));

    let Some(parent_id) = parent_id else {
        return Err(Diagnostic {
            category: "claude_subagent_parent_ambiguous",
            count: 1,
            verbose_path: Some(path.to_path_buf()),
            verbose_chain: None,
        });
    };

    Ok(ChildExecution {
        parent_id,
        agent_id,
        locator: path.to_path_buf(),
        name,
        cwd,
        has_activity,
    })
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
