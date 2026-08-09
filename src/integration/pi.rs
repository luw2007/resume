//! Pi Agent integration.
//!
//! Evidence: Pi 0.84.1.
//! - Agent root: `PI_CODING_AGENT_DIR`, defaulting to `~/.pi/agent`.
//! - Default sessions root: `<agent-root>/sessions`, grouped by encoded
//!   resolved Workspace.
//! - Effective custom session root precedence: `--session-dir`,
//!   `PI_CODING_AGENT_SESSION_DIR`, settings `sessionDir`, then default.
//! - Append-only JSONL with a `type = "session"` header. Current version is 3;
//!   readers also understand v1 and v2. Header fields include stable `id`,
//!   `timestamp`, absolute `cwd`, and optional `parentSession`. A later
//!   `session_info.name` supplies the user display name.
//! - User entries contain `message.role = "user"`, string or typed block
//!   content, and message/entry timestamps.
//! - Pi may migrate old formats when opening them. `resume` parses read-only
//!   and never invokes Pi merely to inspect a Session.
//! - Safest exact Resume is `pi --session <absolute-jsonl-path>`, preserving
//!   `--session-dir <root>` when discovery used a custom root. Do not use
//!   `--session-id`.
//! - `~/.pi/session-control` sockets are positive activity evidence only when
//!   reliably tied to a Session; process presence alone is insufficient.
//!
//! This module never invokes Pi during discovery/preview. It reads JSONL
//! read-only through the shared [`crate::preview::jsonl`] reader, interprets records
//! using the shared [`crate::preview::message`], [`crate::preview::injection`], and
//! [`crate::preview::summary`] helpers, and produces [`Session`] entries and
//! [`ResumeSpec`]s.

use std::{
    ffi::OsString,
    fs, io,
    path::{Path, PathBuf},
    time::SystemTime,
};

use serde_json::Value;

use crate::{
    preview::jsonl::{self, Bounds, FileOutcome, ReadResult},
    preview::message::{self, UserMessage},
    scope::Scope,
    session::{
        ActivityStatus, ResumeSpec, RiskStatus, Session, SessionKey, SupportStatus,
        WorkspaceEvidence,
    },
};

/// Agent name used in [`SessionKey::agent`].
pub const AGENT: &str = "pi";

/// Environment variable overriding the Pi agent root.
pub const ENV_AGENT_DIR: &str = "PI_CODING_AGENT_DIR";
/// Environment variable overriding the Pi session root.
pub const ENV_SESSION_DIR: &str = "PI_CODING_AGENT_SESSION_DIR";

/// Default agent root relative to `$HOME`.
pub const DEFAULT_AGENT_ROOT_RELATIVE: &str = ".pi/agent";
/// Default sessions directory under the agent root.
pub const DEFAULT_SESSIONS_DIR: &str = "sessions";
/// Pi settings file under the agent root.
pub const SETTINGS_FILE: &str = "settings.json";
/// Settings key for the custom session directory.
pub const SETTINGS_SESSION_DIR_KEY: &str = "sessionDir";

/// Maximum number of bytes to scan while extracting header/user-message
/// metadata for discovery. Keeps discovery bounded; full content is available
/// to Preview through the same reader without this ceiling.
const DISCOVERY_SCAN_RECORDS: usize = 50_000;

/// Effective Pi roots resolved from environment and settings, without invoking
/// Pi. The agent root identifies where Pi stores its configuration; the session
/// root is where Session JSONL files live and may be custom (flat) or default
/// (grouped by encoded Workspace).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EffectiveRoots {
    /// The agent root (`<PI_CODING_AGENT_DIR>` or `~/.pi/agent`).
    pub agent_root: PathBuf,
    /// The effective session root: `PI_CODING_AGENT_SESSION_DIR`, settings
    /// `sessionDir`, or the default `<agent-root>/sessions`.
    pub session_root: PathBuf,
    /// Whether the session root is a custom (flat) root rather than the
    /// default grouped layout. Custom roots require header-`cwd` filtering
    /// because directory names are not encoded Workspaces.
    pub custom_session_root: bool,
}

/// Inputs to root resolution, abstracted for testability. Production code uses
/// [`ResolutionInputs::from_env`]; tests inject explicit values.
#[derive(Clone, Debug)]
pub struct ResolutionInputs {
    /// `$HOME`, used to resolve the default agent root.
    pub home: Option<PathBuf>,
    /// `PI_CODING_AGENT_DIR`, overriding the agent root.
    pub agent_dir_env: Option<PathBuf>,
    /// `PI_CODING_AGENT_SESSION_DIR`, overriding the session root.
    pub session_dir_env: Option<PathBuf>,
    /// Explicit `--session-dir` override (highest precedence for the session
    /// root). Discovery callers pass this through when present.
    pub session_dir_flag: Option<PathBuf>,
    /// Parsed `settings.json` content from the agent root, when present.
    pub settings: Option<Value>,
}

impl ResolutionInputs {
    /// Build resolution inputs from the process environment. Reads
    /// `PI_CODING_AGENT_DIR`, `PI_CODING_AGENT_SESSION_DIR`, and `$HOME`. The
    /// caller may attach parsed settings (see [`read_settings`]) before
    /// calling [`resolve`]. Never invokes Pi.
    pub fn from_env() -> Self {
        Self {
            home: std::env::var_os("HOME").map(PathBuf::from),
            agent_dir_env: std::env::var_os(ENV_AGENT_DIR).map(PathBuf::from),
            session_dir_env: std::env::var_os(ENV_SESSION_DIR).map(PathBuf::from),
            session_dir_flag: None,
            settings: None,
        }
    }

    /// Attach parsed settings before resolving.
    pub fn with_settings(mut self, settings: Option<Value>) -> Self {
        self.settings = settings;
        self
    }

    /// Attach an explicit `--session-dir` override before resolving.
    pub fn with_session_dir_flag(mut self, flag: Option<PathBuf>) -> Self {
        self.session_dir_flag = flag;
        self
    }
}

/// Resolve effective Pi roots. Precedence (session root): `--session-dir` flag,
/// then `PI_CODING_AGENT_SESSION_DIR`, then settings `sessionDir`, then the
/// default `<agent-root>/sessions`. Agent root precedence: `PI_CODING_AGENT_DIR`,
/// otherwise `~/.pi/agent`. Never invokes Pi.
///
/// Returns `None` when no agent root can be determined (no `PI_CODING_AGENT_DIR`
/// and no `$HOME`).
pub fn resolve(inputs: &ResolutionInputs) -> Option<EffectiveRoots> {
    let agent_root = agent_root(inputs)?;

    // Session root precedence.
    if let Some(flag) = &inputs.session_dir_flag {
        return Some(EffectiveRoots {
            agent_root,
            session_root: flag.clone(),
            custom_session_root: true,
        });
    }
    if let Some(env_root) = &inputs.session_dir_env {
        return Some(EffectiveRoots {
            agent_root,
            session_root: env_root.clone(),
            custom_session_root: true,
        });
    }
    if let Some(settings) = &inputs.settings
        && let Some(dir) = settings_dir(settings)
    {
        return Some(EffectiveRoots {
            agent_root,
            session_root: dir,
            custom_session_root: true,
        });
    }
    let session_root = agent_root.join(DEFAULT_SESSIONS_DIR);
    Some(EffectiveRoots {
        agent_root,
        session_root,
        custom_session_root: false,
    })
}

/// Resolve the agent root: `PI_CODING_AGENT_DIR` or `~/.pi/agent`.
fn agent_root(inputs: &ResolutionInputs) -> Option<PathBuf> {
    if let Some(env_root) = &inputs.agent_dir_env {
        return Some(env_root.clone());
    }
    inputs
        .home
        .as_deref()
        .map(|home| home.join(DEFAULT_AGENT_ROOT_RELATIVE))
}

/// Extract the `sessionDir` from parsed Pi settings JSON (string or object
/// `{ path }`).
fn settings_dir(settings: &Value) -> Option<PathBuf> {
    match settings.get(SETTINGS_SESSION_DIR_KEY)? {
        Value::String(s) => Some(PathBuf::from(s)),
        Value::Object(obj) => obj.get("path").and_then(|v| v.as_str()).map(PathBuf::from),
        _ => None,
    }
}

/// Read and parse the Pi `settings.json` from an agent root. Returns `None`
/// when absent or unreadable; callers treat absence as "no settings override".
pub fn read_settings(agent_root: &Path) -> Option<Value> {
    let path = agent_root.join(SETTINGS_FILE);
    let text = fs::read_to_string(&path).ok()?;
    serde_json::from_str(&text).ok()
}

/// Configuration for a Pi discovery pass.
#[derive(Clone, Debug)]
pub struct DiscoverConfig<'a> {
    /// Effective Pi roots (owned so callers can pass temporary values).
    pub roots: EffectiveRoots,
    /// Scope used to filter Sessions by header `cwd`.
    pub scope: &'a Scope,
    /// Bounds for the JSONL reader. Discovery uses a record cap.
    pub bounds: Bounds,
}

impl<'a> DiscoverConfig<'a> {
    /// Discovery bounds with the default size limits and a record cap.
    pub fn new(roots: EffectiveRoots, scope: &'a Scope) -> Self {
        let bounds = Bounds {
            max_records: DISCOVERY_SCAN_RECORDS,
            ..Bounds::default()
        };
        Self {
            roots,
            scope,
            bounds,
        }
    }
}

/// A discovered Pi session with its parsed metadata, ready to become a
/// [`Session`] via [`ParsedSession::into_session`]. Holding this separately
/// lets tests inspect extracted fields and the caller dedupe before building
/// final [`Session`] values.
#[derive(Clone, Debug)]
pub struct ParsedSession {
    /// Stable header `id`.
    pub id: String,
    /// Authoritative header `cwd` (Workspace), when present.
    pub workspace: Option<PathBuf>,
    /// Optional `parentSession` id.
    pub parent: Option<String>,
    /// Latest `session_info.name` (user display name).
    pub session_info_name: Option<String>,
    /// Header `timestamp` (epoch seconds).
    pub header_time: Option<SystemTime>,
    /// Latest activity time from message/entry timestamps, then header, then
    /// file mtime.
    pub activity_time: Option<SystemTime>,
    /// Real user messages (terminal-safe, injection-filtered).
    pub messages: Vec<UserMessage>,
    /// Canonical absolute transcript path (the locator used for dedupe and
    /// Resume).
    pub transcript_path: PathBuf,
    /// File mtime fallback for activity.
    pub file_mtime: Option<SystemTime>,
}

impl ParsedSession {
    /// Resolve the title: latest `session_info.name`, else summary from the
    /// first valid human message.
    pub fn title(&self) -> Option<String> {
        if let Some(name) = &self.session_info_name {
            let trimmed = name.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
        let texts: Vec<&str> = self.messages.iter().map(|m| m.text.as_str()).collect();
        crate::preview::summary::summarize_texts(texts, crate::preview::summary::default_width())
    }

    /// Build a [`Session`] from this parsed data.
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
        let title = self.title();
        Session {
            key: SessionKey {
                agent: OsString::from(AGENT),
                effective_root: roots.session_root.clone(),
                profile: None,
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

    /// Build the [`ResumeSpec`]: `pi --session <absolute-jsonl-path>`,
    /// preserving `--session-dir` when discovery used a custom root. Never
    /// uses `--session-id`.
    pub fn resume_spec(&self, roots: &EffectiveRoots) -> ResumeSpec {
        let mut argv: Vec<OsString> = Vec::with_capacity(4);
        argv.push(OsString::from("--session"));
        argv.push(self.transcript_path.clone().into_os_string());
        if roots.custom_session_root {
            argv.push(OsString::from("--session-dir"));
            argv.push(roots.session_root.clone().into_os_string());
        }
        let cwd = self.workspace.clone().unwrap_or_else(|| PathBuf::from("."));
        ResumeSpec {
            program: OsString::from(AGENT),
            argv,
            cwd,
            env: Vec::new(),
        }
    }
}

/// Outcome of discovering Pi sessions in the effective session root.
#[derive(Clone, Debug, Default)]
pub struct DiscoverOutcome {
    /// Parsed sessions, before dedupe and Session construction.
    pub parsed: Vec<ParsedSession>,
    /// Number of JSONL files that were skipped due to read/parse errors
    /// (aggregated, non-fatal).
    pub skipped_files: usize,
    /// Number of files with no valid `session` header.
    pub no_header_files: usize,
    /// Number of files skipped because the header `cwd` was outside Scope.
    pub out_of_scope: usize,
}

/// Discover Pi sessions under the effective session root. Reads JSONL
/// read-only through the shared reader, parses v1/v2/v3 headers, and filters
/// by header `cwd` through Scope. Never invokes Pi or migrates files.
///
/// Discovery scans `.jsonl` files one level under the session root. In the
/// default grouped layout that means `<session-root>/<encoded-workspace>/*.jsonl`;
/// in a custom flat layout it means `<session-root>/*.jsonl`. The caller
/// does not need to know the layout: header `cwd` is authoritative and
/// directory names are never reversed.
pub fn discover(config: &DiscoverConfig<'_>) -> io::Result<DiscoverOutcome> {
    let session_root = config.roots.session_root.clone();
    let confined_root = session_root
        .canonicalize()
        .unwrap_or_else(|_| session_root.clone());
    let mut outcome = DiscoverOutcome::default();
    let mut seen: Vec<(PathBuf, PathBuf)> = Vec::new();

    for jsonl_path in iter_session_files(&session_root)? {
        let parsed = match parse_session_file(&jsonl_path, &confined_root, &config.bounds) {
            Ok(Some(parsed)) => parsed,
            Ok(None) => {
                outcome.no_header_files += 1;
                continue;
            }
            Err(_) => {
                outcome.skipped_files += 1;
                continue;
            }
        };

        // Dedupe: effective session root + canonical transcript locator.
        // canonicalize() requires the file to exist; since we just read it, it
        // does. If canonicalization fails (e.g., the file vanished), fall back
        // to the lexically-absolute path.
        let canonical = jsonl_path
            .canonicalize()
            .unwrap_or_else(|_| jsonl_path.clone());
        let dedupe_key = (config.roots.session_root.clone(), canonical.clone());
        if seen.contains(&dedupe_key) {
            continue;
        }
        seen.push(dedupe_key);

        // Scope filtering via authoritative header cwd.
        match &parsed.workspace {
            Some(workspace) => {
                if !config.scope.contains_workspace(workspace) {
                    outcome.out_of_scope += 1;
                    continue;
                }
            }
            None => {
                // Missing Workspace: discoverable for diagnosis but cannot be
                // resumed (Unavailable). We still surface it.
            }
        }

        outcome.parsed.push(parsed);
    }

    Ok(outcome)
}

/// Enumerate `.jsonl` files reachable from the session root. Tolerates a
/// missing session root (returns empty). Does not follow symlinks for
/// directories but does read symlinked files (the shared reader confines
/// reads to the effective root at the API boundary when requested).
fn iter_session_files(session_root: &Path) -> io::Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    if !session_root.exists() {
        return Ok(paths);
    }
    collect_jsonl(session_root, &mut paths)?;
    // Sort for deterministic discovery order.
    paths.sort();
    Ok(paths)
}

/// Recursively collect `.jsonl` file paths. Bounded by the filesystem; the
/// shared reader imposes record/byte bounds on each file. This recursion is
/// over the storage layout (not Scope), which is permitted: Scope membership
/// is decided later per header `cwd`.
fn collect_jsonl(dir: &Path, out: &mut Vec<PathBuf>) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => continue,
        };
        let path = entry.path();
        let file_type = match entry.file_type() {
            Ok(ft) => ft,
            Err(_) => continue,
        };
        if file_type.is_dir() {
            // Recurse into grouped Workspace directories and flat subdirs.
            collect_jsonl(&path, out)?;
        } else if (file_type.is_file() || file_type.is_symlink())
            && path.extension().and_then(|e| e.to_str()) == Some("jsonl")
        {
            out.push(path);
        }
    }
    Ok(())
}

/// Parse a single Pi JSONL session file read-only. Returns `Ok(None)` when the
/// file has no valid `session` header (not a Pi session transcript).
fn parse_session_file(
    path: &Path,
    effective_root: &Path,
    bounds: &Bounds,
) -> io::Result<Option<ParsedSession>> {
    let result = jsonl::read_file_confined(path, effective_root, bounds)?;
    let file_mtime = fs::metadata(path).and_then(|m| m.modified()).ok();
    Ok(extract_session(path, &result, file_mtime))
}

/// Extract a [`ParsedSession`] from a read result, or `None` if no valid
/// `session` header is present.
fn extract_session(
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
    let parent = header
        .get("parentSession")
        .and_then(|v| v.as_str())
        .map(String::from);
    let header_time = header.get("timestamp").and_then(as_system_time);

    // Latest session_info.name wins (scan in order, keep last non-empty).
    let mut session_info_name: Option<String> = None;
    let mut messages = Vec::new();
    let mut latest_message_time: Option<SystemTime> = None;

    for record in &result.records {
        // session_info records carry a user-facing name.
        if record.get("type").and_then(|v| v.as_str()) == Some("session_info") {
            if let Some(name) = record
                .get("name")
                .and_then(|v| v.as_str())
                .filter(|name| !name.trim().is_empty())
            {
                session_info_name = Some(name.to_string());
            }
            continue;
        }
        // User message records: message.role == "user".
        if let Some(message_obj) = record.get("message").and_then(|v| v.as_object())
            && message_obj.get("role").and_then(|v| v.as_str()) == Some("user")
            && let Some(msg) = extract_user_message(message_obj)
        {
            // Track the latest activity time from message/entry timestamps.
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

    Some(ParsedSession {
        id,
        workspace,
        parent,
        session_info_name,
        header_time,
        activity_time,
        messages,
        transcript_path: path.to_path_buf(),
        file_mtime,
    })
}

/// Find the first record with `type == "session"`. Pi v1/v2/v3 all use a
/// `session` header record; the version field is advisory and does not change
/// the fields we extract. An unknown or missing header means this is not a Pi
/// session transcript.
fn find_session_header(records: &[Value]) -> Option<&Value> {
    records
        .iter()
        .find(|record| record.get("type").and_then(|v| v.as_str()) == Some("session"))
}

/// Extract a [`UserMessage`] from a `message` object with `role == "user"`.
/// Handles string content, typed block content (text/image/file), and produces
/// safe attachment placeholders (never base64).
fn extract_user_message(message_obj: &serde_json::Map<String, Value>) -> Option<UserMessage> {
    let content = message_obj
        .get("content")
        .or_else(|| message_obj.get("text"));
    let (text, attachments) = match content {
        Some(value) => message::extract_content(value),
        None => (None, Vec::new()),
    };
    // A user message with neither text nor attachments is not useful evidence.
    if text.as_ref().is_some_and(|t| !t.trim().is_empty()) || !attachments.is_empty() {
        Some(message::build_user_message(text, attachments))
    } else {
        None
    }
}

/// Convert a JSON timestamp to `SystemTime`. See `crate::time::json_value_to_system_time`.
fn as_system_time(value: &Value) -> Option<SystemTime> {
    crate::time::json_value_to_system_time(value)
}

/// Determine the [`ActivityStatus`] for a parsed session given optional
/// positive-evidence Session Control association. Active is reported only after
/// a validated stable-ID/path association; otherwise Unknown. Absence of
/// evidence is Unknown, never Inactive.
pub fn activity_status(
    parsed: &ParsedSession,
    control_evidence: Option<&SessionControlEvidence>,
) -> ActivityStatus {
    match control_evidence {
        Some(evidence) if evidence.matches(parsed) => ActivityStatus::Active {
            observed_at: evidence.observed_at,
        },
        _ => ActivityStatus::Unknown,
    }
}

/// Positive-evidence association between a live `session-control` entry and a
/// parsed session. A match requires the stable ID to agree with the control
/// entry and the control entry's transcript path to resolve to the parsed
/// session's transcript locator.
#[derive(Clone, Debug)]
pub struct SessionControlEvidence {
    /// Stable session ID recorded in the control entry.
    pub session_id: String,
    /// Transcript path recorded in the control entry.
    pub transcript_path: PathBuf,
    /// When the association was observed.
    pub observed_at: SystemTime,
}

impl SessionControlEvidence {
    fn matches(&self, parsed: &ParsedSession) -> bool {
        if self.session_id != parsed.id {
            return false;
        }
        // The control entry's transcript path must resolve to the same file.
        let self_canon = self.transcript_path.canonicalize().ok();
        let parsed_canon = parsed.transcript_path.canonicalize().ok();
        match (self_canon, parsed_canon) {
            (Some(a), Some(b)) => a == b,
            // Fall back to lexical equality if canonicalization fails.
            _ => self.transcript_path == parsed.transcript_path,
        }
    }
}

/// Compute risk status for a parsed Pi session, including the broad-workspace
/// check against `$HOME`/`/`.
pub fn risk_status(parsed: &ParsedSession, home: Option<&Path>) -> RiskStatus {
    let evidence = match &parsed.workspace {
        Some(workspace) => WorkspaceEvidence::Recorded {
            workspace: workspace.clone(),
            historical_git_identity: None,
        },
        None => return RiskStatus::Normal,
    };
    crate::scope::broad_workspace_risk(&evidence, home)
}

/// Whether a read result indicates a file that was being actively written
/// (incomplete tail). Useful for tests and diagnostics; does not affect
/// discovery correctness since we retain valid records regardless.
pub fn was_live_growing(result: &ReadResult) -> bool {
    matches!(result.outcome, FileOutcome::IncompleteTail)
}

// ---------------------------------------------------------------------------
// Test-exposed wrappers. These are `#[doc(hidden)]` public functions so the
// integration test module (a sibling file) can reach private helpers for
// focused assertions. They are not part of the public API.
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
pub fn settings_dir_pub(settings: &Value) -> Option<PathBuf> {
    settings_dir(settings)
}

#[cfg(test)]
mod tests;
