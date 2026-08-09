//! OMP record parsing and Session construction.

use super::{AGENT, roots::EffectiveRoots};
use crate::{
    jsonl::ReadResult,
    message::{self, UserMessage},
    scope,
    session::{ActivityStatus, RiskStatus, Session, SessionKey, SupportStatus, WorkspaceEvidence},
};
use serde_json::Value;
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    time::SystemTime,
};

/// A safe origin badge for an imported Session. The origin Codex/Claude Session
/// is never merged with the new OMP Session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImportBadge {
    /// Source agent kind (e.g. "codex", "claude").
    pub source_kind: String,
    /// Origin agent's native session ID, if recorded.
    pub origin_id: Option<String>,
    /// Origin Workspace, if recorded. Used only for the badge display.
    pub origin_cwd: Option<PathBuf>,
}

impl ImportBadge {
    /// A safe, short display string for the badge. Never exposes the origin
    /// repository remote or an absolute import source path verbatim.
    pub fn to_display(&self) -> String {
        let mut parts = vec![format!("imported from {}", self.source_kind)];
        if let Some(id) = &self.origin_id {
            // Show only a short prefix of the origin ID to avoid impersonating
            // it as a resumable locator.
            let short: String = id.chars().take(8).collect();
            parts.push(format!("origin:{short}"));
        }
        parts.join(" ")
    }
}

/// A discovered OMP session with its parsed metadata, ready to become a
/// [`Session`] via [`ParsedSession::into_session`].
#[derive(Clone, Debug)]
pub struct ParsedSession {
    /// Stable header `id` (the new OMP ID for imports).
    pub id: String,
    /// Authoritative header `cwd` (Workspace), when present.
    pub workspace: Option<PathBuf>,
    /// Header `timestamp` (epoch seconds).
    pub header_time: Option<SystemTime>,
    /// Resolved effective title across header/title/title_change state.
    pub title: Option<String>,
    /// Real user messages (terminal-safe, injection-filtered, attribution-aware).
    pub messages: Vec<UserMessage>,
    /// Canonical absolute transcript path (the locator used for dedupe).
    pub transcript_path: PathBuf,
    /// File mtime fallback for activity.
    pub file_mtime: Option<SystemTime>,
    /// Latest activity time from message/title timestamps, then header, then mtime.
    pub activity_time: Option<SystemTime>,
    /// Import badge, when this Session is an imported foreign Session.
    pub import: Option<ImportBadge>,
}

impl ParsedSession {
    /// Build a [`Session`] from this parsed data. Profile and effective root
    /// are embedded in the key so that duplicate IDs across profiles never
    /// collide.
    pub fn into_session(
        self,
        roots: &EffectiveRoots,
        risk: RiskStatus,
        activity: ActivityStatus,
    ) -> Session {
        let workspace_evidence = match &self.workspace {
            Some(workspace) => WorkspaceEvidence::Recorded {
                workspace: workspace.clone(),
                historical_git_identity: None,
            },
            None => WorkspaceEvidence::Unknown,
        };
        let title = match (self.title, self.import) {
            (Some(title), Some(import)) => Some(format!("{title} [{}]", import.to_display())),
            (None, Some(import)) => Some(import.to_display()),
            (title, None) => title,
        };
        Session {
            key: SessionKey {
                agent: OsString::from(AGENT),
                effective_root: roots.session_root.clone(),
                profile: roots.profile.as_profile_field(),
                native_locator: self.transcript_path.clone().into_os_string(),
            },
            resumable_id: OsString::from(self.id),
            title,
            workspace: workspace_evidence,
            support: SupportStatus::Supported,
            activity,
            risk,
        }
    }
}

/// Title state accumulated from the title sidecar, the v3 header, and any
/// `title_change` records. Latest non-empty title wins.
#[derive(Clone, Debug, Default)]
struct TitleState {
    current: Option<String>,
}

impl TitleState {
    fn set(&mut self, title: Option<String>) {
        if let Some(t) = title
            && !t.trim().is_empty()
        {
            self.current = Some(t);
        }
    }
}

/// Extract a [`ParsedSession`] from a read result, or `None` if no valid
/// `session` header is present. Parses the title sidecar that may precede the
/// v3 header without assuming any record position.
pub(super) fn extract_session(
    path: &Path,
    result: &ReadResult,
    file_mtime: Option<SystemTime>,
) -> Option<ParsedSession> {
    let header = find_session_header(&result.records)?;
    let id = header.get("id").and_then(|v| v.as_str())?.to_string();
    let workspace = header
        .get("cwd")
        .and_then(|v| v.as_str())
        .map(PathBuf::from);
    let header_time = header.get("timestamp").and_then(as_system_time);

    let mut title_state = TitleState::default();

    let mut messages = Vec::new();
    let mut latest_message_time: Option<SystemTime> = None;
    let mut import: Option<ImportBadge> = None;

    for record in &result.records {
        let rec_type = record.get("type").and_then(|v| v.as_str());

        // The v3 session header itself: extract its title metadata in record
        // order so that a title sidecar before it, the header, and a later
        // title_change all apply positionally (latest non-empty wins).
        if rec_type == Some("session") {
            title_state.set(
                record
                    .get("title")
                    .and_then(|v| v.as_str())
                    .map(String::from),
            );
            continue;
        }

        // Title sidecar: type == "title", v == 1 (padded). Applies before or
        // after the header positionally; we process in record order so a
        // later title_change can override it.
        if rec_type == Some("title") {
            title_state.set(
                record
                    .get("title")
                    .or_else(|| record.get("text"))
                    .and_then(|v| v.as_str())
                    .map(String::from),
            );
            continue;
        }

        // title_change records update title state.
        if rec_type == Some("title_change") {
            title_state.set(
                record
                    .get("title")
                    .and_then(|v| v.as_str())
                    .map(String::from),
            );
            continue;
        }

        // foreign_session_import custom entry → safe origin badge only.
        if rec_type == Some("custom") {
            if let Some(import_val) = record.get("foreign_session_import") {
                import = parse_import(import_val);
            }
            continue;
        }

        // User message records: message.role == "user", attribution-aware.
        if let Some(message_obj) = record.get("message").and_then(|v| v.as_object())
            && message_obj.get("role").and_then(|v| v.as_str()) == Some("user")
            && is_user_attributed(record, message_obj)
            && let Some(msg) = extract_user_message(message_obj)
        {
            let t = record
                .get("timestamp")
                .and_then(as_system_time)
                .or_else(|| message_obj.get("timestamp").and_then(as_system_time));
            if let Some(t) = t {
                latest_message_time = Some(match latest_message_time {
                    Some(current) if current >= t => current,
                    _ => t,
                });
            }
            messages.push(msg);
        }
    }

    let activity_time = latest_message_time.or(header_time).or(file_mtime);

    // Resolve effective title: accumulated title state, else summary from the
    // first valid human message.
    let title = title_state.current.or_else(|| {
        let texts: Vec<&str> = messages.iter().map(|m| m.text.as_str()).collect();
        crate::summary::summarize_texts(texts, crate::summary::default_width())
    });

    Some(ParsedSession {
        id,
        workspace,
        header_time,
        title,
        messages,
        transcript_path: path.to_path_buf(),
        file_mtime,
        activity_time,
        import,
    })
}

/// Find the first record with `type == "session"`. Unlike Pi, we make no
/// assumption that this is the first record: OMP may precede it with a padded
/// title sidecar.
fn find_session_header(records: &[Value]) -> Option<&Value> {
    records
        .iter()
        .find(|record| record.get("type").and_then(|v| v.as_str()) == Some("session"))
}

/// Determine whether a `message.role == "user"` record is genuinely
/// user-attributed, filtering out agent-injected inputs. OMP records may carry
/// an attribution field (e.g. `attribution.source`, `message.source`,
/// `meta.source`) marking injected/automated messages. Anything attributed to
/// the agent, the system, or a tool is excluded.
fn is_user_attributed(record: &Value, message_obj: &serde_json::Map<String, Value>) -> bool {
    // Check common attribution locations.
    let sources = [
        record.get("attribution").and_then(|v| v.get("source")),
        record.get("source"),
        message_obj.get("attribution").and_then(|v| v.get("source")),
        message_obj.get("source"),
        record.get("meta").and_then(|v| v.get("source")),
    ];
    for source in sources.into_iter().flatten() {
        if let Some(s) = source.as_str() {
            let lower = s.to_ascii_lowercase();
            // Reject agent/system/tool/injected/auto attributions.
            if lower == "agent"
                || lower == "assistant"
                || lower == "system"
                || lower == "tool"
                || lower == "injected"
                || lower == "auto"
                || lower == "automated"
            {
                return false;
            }
        }
        // A boolean "injected"/"automated" flag also excludes.
        if source == true {
            // Only treat bare `true` as exclusion if the key context implies it;
            // to be safe we look at the containing object key.
        }
    }
    // Explicit injected/automated boolean flags.
    let flags = [
        record.get("injected"),
        record.get("automated"),
        message_obj.get("injected"),
        message_obj.get("automated"),
        record.get("meta").and_then(|v| v.get("injected")),
    ];
    for flag in flags.into_iter().flatten() {
        if flag == true {
            return false;
        }
    }
    true
}

/// Extract a [`UserMessage`] from a `message` object with `role == "user"`.
fn extract_user_message(message_obj: &serde_json::Map<String, Value>) -> Option<UserMessage> {
    let content = message_obj
        .get("content")
        .or_else(|| message_obj.get("text"));
    let (text, attachments) = match content {
        Some(value) => message::extract_content(value),
        None => (None, Vec::new()),
    };
    if text.as_ref().is_some_and(|t| !t.trim().is_empty()) || !attachments.is_empty() {
        Some(message::build_user_message(text, attachments))
    } else {
        None
    }
}

/// Parse a `foreign_session_import` value into a safe [`ImportBadge`].
/// Origin repository remotes and absolute import source paths are not exposed.
fn parse_import(value: &Value) -> Option<ImportBadge> {
    let obj = value.as_object()?;
    let source_kind = obj
        .get("source_kind")
        .or_else(|| obj.get("kind"))
        .or_else(|| obj.get("source"))
        .and_then(|v| v.as_str())?
        .to_string();
    let origin_id = obj
        .get("origin_id")
        .or_else(|| obj.get("source_id"))
        .or_else(|| obj.get("original_id"))
        .and_then(|v| v.as_str())
        .map(String::from);
    let origin_cwd = obj
        .get("origin_cwd")
        .or_else(|| obj.get("source_cwd"))
        .or_else(|| obj.get("cwd"))
        .and_then(|v| v.as_str())
        .map(PathBuf::from);
    Some(ImportBadge {
        source_kind,
        origin_id,
        origin_cwd,
    })
}

/// Convert a JSON timestamp to `SystemTime`. See `crate::time::json_value_to_system_time`.
fn as_system_time(value: &Value) -> Option<SystemTime> {
    crate::time::json_value_to_system_time(value)
}
/// check against `$HOME`/`/`.
pub fn risk_status(parsed: &ParsedSession, home: Option<&Path>) -> RiskStatus {
    let evidence = match &parsed.workspace {
        Some(workspace) => WorkspaceEvidence::Recorded {
            workspace: workspace.clone(),
            historical_git_identity: None,
        },
        None => return RiskStatus::Normal,
    };
    scope::broad_workspace_risk(&evidence, home)
}
// ---------------------------------------------------------------------------
// Test-exposed wrappers.
// ---------------------------------------------------------------------------

#[doc(hidden)]
pub fn extract_session_pub(
    path: &Path,
    result: &ReadResult,
    file_mtime: Option<SystemTime>,
) -> Option<ParsedSession> {
    extract_session(path, result, file_mtime)
}

#[doc(hidden)]
pub fn parse_import_pub(value: &Value) -> Option<ImportBadge> {
    parse_import(value)
}
