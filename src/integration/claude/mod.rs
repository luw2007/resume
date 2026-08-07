//! Claude Code Agent Integration.
//!
//! Evidence: Claude Code 2.1.220, root `CLAUDE_CONFIG_DIR` (default `~/.claude`),
//! heterogeneous top-level JSONL transcripts under `projects/<workspace-key>/`.
//! The workspace key replaces non-alphanumeric characters and is collision-prone;
//! it is **never** reversed to infer Workspace. Per-record event `cwd` is the
//! authoritative Workspace source.
//!
//! ## Identity contract
//!
//! Accept a stable identity only when the UUID filename and the embedded
//! `sessionId` agree. Per-record `uuid` is not the resumable Session ID. When
//! filename and embedded ID disagree (or the embedded ID is absent), the
//! Session is diagnosed/skipped or marked Discover Only according to the
//! evidence available — it is never silently resumed under a guessed ID.
//!
//! ## Title precedence
//!
//! Explicit agent/display name, then AI title, then a deterministic summary of
//! the first valid human text input. This precedence is **Resume presentation**,
//! not a claim of native picker equivalence — the native Claude title precedence
//! was not safely invoked in research.
//!
//! ## Resume contract
//!
//! `claude --resume <uuid>` with the authoritative Workspace as the child cwd,
//! preserving a nondefault `CLAUDE_CONFIG_DIR`. Never `--continue` for exact
//! Resume.
//!
//! ## Activity
//!
//! No authoritative active marker was found. Activity is [`ActivityStatus::Unknown`]
//! absent a future validated positive process/session association.

use std::{
    ffi::{OsStr, OsString},
    fs,
    path::{Path, PathBuf},
};

use serde_json::Value;

use crate::{
    jsonl::{self, Bounds, FileOutcome, ReadResult},
    message,
    session::{
        ActivityStatus, Diagnostic, IntegrationError, ResumeSpec, Session, SessionKey,
        SupportStatus, WorkspaceEvidence,
    },
    summary,
};

/// Agent name used in [`SessionKey::agent`] and the Resume `program`.
pub const AGENT: &str = "claude";

/// Environment variable that overrides the Claude config root.
pub const CONFIG_DIR_ENV: &str = "CLAUDE_CONFIG_DIR";

/// Subdirectory under the config root that holds per-workspace transcripts.
const PROJECTS_DIR: &str = "projects";

/// Summary width for the deterministic fallback title.
const SUMMARY_WIDTH: usize = 80;

/// An effective Claude root plus whether it came from a nondefault
/// `CLAUDE_CONFIG_DIR`.
///
/// `effective_root` is the resolved config root (`CLAUDE_CONFIG_DIR` or
/// `~/.claude`). It is part of Session provenance and identity: two roots with
/// identical transcripts are distinct Sessions. `nondefault` controls whether
/// the root is preserved on Resume as an environment override.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClaudeRoot {
    pub effective_root: PathBuf,
    pub nondefault: bool,
}

/// A discovered Claude transcript candidate, before parsing and Scope filtering.
struct Candidate {
    /// Canonical real path of the `.jsonl` transcript.
    path: PathBuf,
    /// The stem (filename without extension), expected to be a UUID.
    stem: OsString,
}

/// Result of discovering Claude Sessions under a resolved root, before Scope
/// filtering. Each [`Session`] has its recorded Workspace populated from event
/// `cwd`; Scope membership is applied by the caller.
#[derive(Clone, Debug)]
pub struct Discovery {
    pub sessions: Vec<Session>,
    pub diagnostics: Vec<Diagnostic>,
}

impl Discovery {
    fn new() -> Self {
        Self {
            sessions: Vec::new(),
            diagnostics: Vec::new(),
        }
    }
}

/// Resolve the effective Claude root from the environment and home directory.
///
/// Returns `None` when no root can be established (no `CLAUDE_CONFIG_DIR` and
/// no `HOME`). This function performs no filesystem mutation and never invokes
/// the Claude CLI.
pub fn resolve_root(config_dir_env: Option<&OsStr>, home: Option<&Path>) -> Option<ClaudeRoot> {
    if let Some(explicit) = config_dir_env
        && !explicit.is_empty()
    {
        return Some(ClaudeRoot {
            effective_root: PathBuf::from(explicit),
            nondefault: true,
        });
    }
    home.map(|home| ClaudeRoot {
        effective_root: home.join(".claude"),
        nondefault: false,
    })
}

/// Discover Claude Sessions under a resolved root.
///
/// Scans only valid top-level Session transcripts (direct `.jsonl` children of
/// each workspace-key directory), excluding nested subagent artifacts. Each
/// retained Session carries the recorded `cwd` as its Workspace evidence so
/// callers can apply Scope membership. Read-only: no file is opened for write,
/// no directory entry or mtime is changed, and the Claude CLI is never invoked.
pub fn discover(root: &ClaudeRoot) -> Result<Discovery, IntegrationError> {
    let projects = root.effective_root.join(PROJECTS_DIR);
    let projects_real = match projects.canonicalize() {
        Ok(path) => path,
        Err(_) => {
            // No `projects` directory: nothing to discover, not an error.
            return Ok(Discovery::new());
        }
    };

    let mut discovery = Discovery::new();
    let candidates = collect_candidates(&projects_real, &mut discovery.diagnostics);

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
fn collect_candidates(projects: &Path, diagnostics: &mut Vec<Diagnostic>) -> Vec<Candidate> {
    let mut candidates = Vec::new();
    let workspace_dirs = match fs::read_dir(projects) {
        Ok(entries) => entries,
        Err(_) => return candidates,
    };

    for entry in workspace_dirs.flatten() {
        if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
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
                    diagnostics.push(diagnostic_count(
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

/// A parsed transcript's extracted state.
#[derive(Clone, Debug)]
struct ParsedTranscript {
    /// The embedded `sessionId`, when present.
    session_id: Option<String>,
    /// The first authoritative `cwd` seen across events.
    cwd: Option<PathBuf>,
    /// Explicit agent/display name (e.g. `agent-name` / `agentName`).
    agent_name: Option<String>,
    /// AI-generated title (e.g. `ai-title` / `aiTitle`).
    ai_title: Option<String>,
    /// Real (human) user messages, in order of appearance.
    user_messages: Vec<message::UserMessage>,
}

/// Parse a candidate transcript file into extracted state and the raw read
/// outcome (for diagnostics). Uses the shared bounded JSONL reader.
fn parse_transcript(path: &Path) -> Result<(ParsedTranscript, ReadResult), Diagnostic> {
    let read = jsonl::read_file(path, &Bounds::default()).map_err(|source| {
        diagnostic_chain(
            "claude_io",
            path,
            &format!("failed to read transcript: {source}"),
        )
    })?;

    let mut parsed = ParsedTranscript {
        session_id: None,
        cwd: None,
        agent_name: None,
        ai_title: None,
        user_messages: Vec::new(),
    };

    for record in &read.records {
        interpret_record(record, &mut parsed);
    }

    Ok((parsed, read))
}

/// Interpret a single heterogeneous record. Unknown records are accepted and
/// ignored (adapter dispatch only). This never assumes a fixed schema version
/// or a header position.
fn interpret_record(record: &Value, parsed: &mut ParsedTranscript) {
    // Session ID: the canonical spelling in observed transcripts is
    // `sessionId`. Per-record `uuid` is explicitly NOT the resumable ID.
    if parsed.session_id.is_none()
        && let Some(id) = record
            .get("sessionId")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
    {
        parsed.session_id = Some(id.to_string());
    }

    // `cwd` may appear on many record types; take the first non-empty value.
    // This is the authoritative Workspace — the workspace-key directory name
    // is never reversed.
    if parsed.cwd.is_none()
        && let Some(cwd) = record.get("cwd").and_then(Value::as_str)
        && !cwd.is_empty()
    {
        parsed.cwd = Some(PathBuf::from(cwd));
    }

    // Explicit agent/display name. Both snake_case and camelCase spellings
    // appear across producer versions.
    if parsed.agent_name.is_none() {
        parsed.agent_name = first_nonempty_str(record, &["agent-name", "agentName"]);
    }

    // AI-generated title. Both spellings appear.
    if parsed.ai_title.is_none() {
        parsed.ai_title = first_nonempty_str(record, &["ai-title", "aiTitle"]);
    }

    // User content extraction. Only `type: "user"` records with string or text
    // block content are human input. Exclude `tool_result` blocks, assistant,
    // system, and injected records.
    let record_type = record.get("type").and_then(Value::as_str);
    if record_type == Some("user")
        && !is_injected(record)
        && let Some(content) = record
            .get("message")
            .and_then(|m| m.get("content"))
            .or_else(|| record.get("content"))
        && has_human_input(content)
    {
        let (text, attachments) = message::extract_content(content);
        let msg = message::build_user_message(text, attachments);
        if !msg.text.trim().is_empty() || !msg.attachments.is_empty() {
            parsed.user_messages.push(msg);
        }
    }
}

/// Whether a content value (string or array of blocks) contains any human
/// text or non-`tool_result` block. A `user` record whose content is entirely
/// `tool_result` blocks is tool output, not human input.
fn has_human_input(content: &Value) -> bool {
    match content {
        Value::String(s) => !s.is_empty(),
        Value::Array(blocks) => blocks.iter().any(|block| {
            let kind = block.get("type").and_then(Value::as_str);
            kind != Some("tool_result")
        }),
        Value::Object(map) => {
            map.get("type").and_then(Value::as_str) != Some("tool_result") && !map.is_empty()
        }
        _ => false,
    }
}

/// Whether a user record is agent-injected (not authored by the human). Some
/// producer versions mark sidechain/meta content; we exclude such records.
fn is_injected(record: &Value) -> bool {
    record
        .get("isMeta")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || record
            .get("isSidechain")
            .and_then(Value::as_bool)
            .unwrap_or(false)
}

/// Return the first non-empty string value among the given keys.
fn first_nonempty_str(record: &Value, keys: &[&str]) -> Option<String> {
    for key in keys {
        if let Some(value) = record.get(*key).and_then(Value::as_str)
            && !value.is_empty()
        {
            return Some(value.to_string());
        }
    }
    None
}

/// Build a [`Session`] from a candidate + parsed transcript, applying the
/// identity contract (UUID filename == embedded sessionId).
///
/// Returns `(Option<Session>, Vec<Diagnostic>)`: `Some(session)` plus any
/// non-fatal diagnostics (truncation/malformed) when retained, or `None` plus
/// diagnostics when skipped. A hard I/O failure is returned as `Err`.
fn parse_candidate(
    candidate: &Candidate,
    root: &ClaudeRoot,
) -> Result<(Option<Session>, Vec<Diagnostic>), Diagnostic> {
    let (parsed, read) = parse_transcript(&candidate.path)?;

    let filename_uuid_str = candidate.stem.to_string_lossy().into_owned();

    // Identity contract: filename UUID and embedded sessionId must agree.
    let (stable_id, support) = match &parsed.session_id {
        Some(embedded) => {
            if !uuid_agrees(&filename_uuid_str, embedded) {
                // Disagreement: cannot safely resume under either ID. If there
                // is no authoritative Workspace, skip entirely; otherwise
                // surface as Discover Only so the user sees it without a
                // resumable contract.
                if parsed.cwd.is_none() {
                    return Err(diagnostic_chain(
                        "claude_identity_disagreement",
                        &candidate.path,
                        "filename and embedded sessionId disagree and no cwd; skipped",
                    ));
                }
                (
                    OsString::from(embedded.clone()),
                    SupportStatus::DiscoverOnly,
                )
            } else {
                (OsString::from(embedded.clone()), SupportStatus::Supported)
            }
        }
        None => {
            // No embedded sessionId: the filename alone is not authoritative.
            // Distinguish UUID-shaped filenames (plausible but unconfirmed)
            // from non-UUID filenames (weak identity). In both cases the
            // result is Discover Only when a cwd exists, never Supported.
            if parsed.cwd.is_none() {
                return Err(diagnostic_chain(
                    "claude_no_session_id",
                    &candidate.path,
                    "no embedded sessionId and no cwd; skipped",
                ));
            }
            // Unconfirmed filename identity: Discover Only.
            (candidate.stem.clone(), SupportStatus::DiscoverOnly)
        }
    };

    // Workspace evidence from authoritative event `cwd`. Never inferred from
    // the workspace-key directory name.
    let workspace = match parsed.cwd.clone() {
        Some(cwd) => WorkspaceEvidence::Recorded {
            workspace: cwd,
            historical_git_identity: None,
        },
        None => WorkspaceEvidence::Unknown,
    };

    // Title precedence: explicit agent/display name, then AI title, then a
    // deterministic summary of the first valid human input. This is Resume
    // presentation, not a claim of native picker equivalence.
    let title = parsed
        .agent_name
        .clone()
        .or_else(|| parsed.ai_title.clone())
        .or_else(|| {
            summary::summarize_texts(
                parsed
                    .user_messages
                    .iter()
                    .filter(|message| !message.text.is_empty())
                    .map(|message| message.text.as_str()),
                SUMMARY_WIDTH,
            )
        });

    let key = SessionKey {
        agent: OsString::from(AGENT),
        effective_root: root.effective_root.clone(),
        profile: None,
        native_locator: candidate.path.clone().into_os_string(),
    };

    // Non-fatal diagnostics: a non-Complete outcome or malformed middle
    // records are surfaced but do not block discovery of an otherwise-valid
    // transcript.
    let mut nonfatal = Vec::new();
    // If identity came from a non-UUID filename (no embedded sessionId), note
    // the weak identity provenance for diagnostics.
    if parsed.session_id.is_none() && !looks_like_uuid(&filename_uuid_str) {
        nonfatal.push(diagnostic_chain(
            "claude_weak_identity",
            &candidate.path,
            "no embedded sessionId and filename is not UUID-shaped; identity unconfirmed",
        ));
    }
    match read.outcome {
        FileOutcome::Complete => {}
        FileOutcome::IncompleteTail => nonfatal.push(diagnostic_count(
            "claude_truncated",
            &candidate.path,
            "transcript ended with an incomplete record",
        )),
        FileOutcome::BoundExceeded => nonfatal.push(diagnostic_count(
            "claude_oversized",
            &candidate.path,
            "transcript exceeded a read bound",
        )),
    }
    if read.malformed_middle > 0 {
        nonfatal.push(diagnostic_count(
            "claude_malformed",
            &candidate.path,
            &format!(
                "{} malformed middle record(s) skipped",
                read.malformed_middle
            ),
        ));
    }

    let session = Session {
        key,
        resumable_id: stable_id,
        title,
        workspace,
        support,
        activity: ActivityStatus::Unknown,
        risk: crate::session::RiskStatus::Normal,
    };

    Ok((Some(session), nonfatal))
}

/// Build the exact Resume spec for a Claude Session.
///
/// `claude --resume <uuid>` with the authoritative Workspace as the child cwd.
/// A nondefault `CLAUDE_CONFIG_DIR` is preserved as an environment override.
/// Never `--continue`.
pub fn resume_spec(session: &Session, root: &ClaudeRoot) -> Result<ResumeSpec, IntegrationError> {
    let workspace =
        session
            .workspace
            .workspace()
            .ok_or_else(|| IntegrationError::InvalidSession {
                diagnostic: Diagnostic {
                    category: "claude_missing_workspace",
                    count: 1,
                    verbose_path: None,
                    verbose_chain: Some("no recorded cwd; cannot resume".into()),
                },
            })?;

    let argv = vec![OsString::from("--resume"), session.resumable_id.clone()];

    let env = if root.nondefault {
        vec![(
            OsString::from(CONFIG_DIR_ENV),
            root.effective_root.clone().into_os_string(),
        )]
    } else {
        Vec::new()
    };

    Ok(ResumeSpec {
        program: OsString::from(AGENT),
        argv,
        cwd: workspace.to_path_buf(),
        env,
    })
}

/// Check whether a filename UUID and an embedded sessionId agree. Comparison
/// is case-insensitive and ignores surrounding braces.
fn uuid_agrees(filename: &str, embedded: &str) -> bool {
    normalize_uuid(filename) == normalize_uuid(embedded)
}

/// Normalize a UUID string for comparison: strip `{` `}` and lowercase.
fn normalize_uuid(value: &str) -> String {
    value
        .trim_matches(|c| c == '{' || c == '}')
        .to_ascii_lowercase()
}

/// Validate that a string looks like a UUID (8-4-4-4-12 hex).
fn looks_like_uuid(value: &str) -> bool {
    let trimmed = value.trim_matches(|c| c == '{' || c == '}');
    let groups = [8usize, 4, 4, 4, 12];
    let mut idx = 0;
    for (i, &expected) in groups.iter().enumerate() {
        let chunk = match trimmed.get(idx..idx + expected) {
            Some(chunk) => chunk,
            None => return false,
        };
        if !chunk.bytes().all(|b| b.is_ascii_hexdigit()) {
            return false;
        }
        idx += expected;
        if i + 1 < groups.len() {
            match trimmed.as_bytes().get(idx) {
                Some(b'-') => idx += 1,
                _ => return false,
            }
        }
    }
    idx == trimmed.len()
}

// --- diagnostic helpers ---

fn diagnostic_count(category: &'static str, path: &Path, _note: &str) -> Diagnostic {
    Diagnostic {
        category,
        count: 1,
        verbose_path: Some(path.to_path_buf()),
        verbose_chain: None,
    }
}

fn diagnostic_chain(category: &'static str, path: &Path, chain: &str) -> Diagnostic {
    Diagnostic {
        category,
        count: 1,
        verbose_path: Some(path.to_path_buf()),
        verbose_chain: Some(chain.to_string()),
    }
}

#[cfg(test)]
mod tests;
