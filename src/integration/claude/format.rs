use super::{
    AGENT,
    discover::{Candidate, diagnostic_chain, diagnostic_count},
    resume::{looks_like_uuid, uuid_agrees},
    roots::ClaudeRoot,
};
use crate::{
    preview::jsonl::{self, Bounds, FileOutcome, ReadResult},
    preview::message,
    preview::summary,
    session::{
        ActivityStatus, Diagnostic, Session, SessionKey, SupportStatus, UpdateTime,
        UpdateTimeSource, WorkspaceEvidence,
    },
};
use serde_json::Value;
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};
const SUMMARY_WIDTH: usize = 80;
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
fn parse_transcript(
    path: &Path,
    effective_root: &Path,
) -> Result<(ParsedTranscript, ReadResult), Diagnostic> {
    let read =
        jsonl::read_file_confined(path, effective_root, &Bounds::default()).map_err(|source| {
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
pub(super) fn parse_candidate(
    candidate: &Candidate,
    root: &ClaudeRoot,
) -> Result<(Option<Session>, Vec<Diagnostic>), Diagnostic> {
    let confined_root = root
        .effective_root
        .canonicalize()
        .unwrap_or_else(|_| root.effective_root.clone());
    let (parsed, read) = parse_transcript(&candidate.path, &confined_root)?;

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
        updated_at: std::fs::metadata(&candidate.path)
            .and_then(|metadata| metadata.modified())
            .ok()
            .map(|at| UpdateTime {
                at,
                source: UpdateTimeSource::FileMtime,
            }),
        workspace,
        support,
        activity: ActivityStatus::Unknown,
        risk: crate::session::RiskStatus::Normal,
    };

    Ok((Some(session), nonfatal))
}

#[cfg(test)]
#[path = "tests/format.rs"]
mod tests;
