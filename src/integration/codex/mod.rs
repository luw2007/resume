//! Codex JSONL Agent Integration (Step 6).
//!
//! Discovers Codex Sessions from rollout JSONL under the effective `CODEX_HOME`
//! (defaulting to `~/.codex`), without depending on SQLite (`state_5.sqlite`)
//! or legacy indexes (`session_index.jsonl`, `history.jsonl`). The rollout JSONL
//! is authoritative for identity and Workspace.
//!
//! ## Identity and Workspace
//!
//! The stable identity is `session_meta.payload.id`, **not** the filename and
//! **not** the unrelated `payload.session_id`. The Workspace is
//! `session_meta.payload.cwd`, never the storage directory or
//! `workspace_roots`.
//!
//! ## User message extraction
//!
//! User input may appear twice in a rollout: as an `event_msg` record with
//! `payload.type = "user_message"`, and as a `response_item` record whose
//! message has `role = "user"`. The two representations are deduplicated.
//! Developer/system injections and environmental context are excluded.
//!
//! ## Source/import badges
//!
//! `source`, `thread_source`, `parent_thread_id`, and import metadata are
//! preserved only as safe badges. The repository remote and import source path
//! are never displayed by default.
//!
//! ## Resume
//!
//! [`codex -C <workspace> resume <uuid>`][resume], preserving a nondefault
//! `CODEX_HOME` via the environment. Codex 0.146.0 does not document Resume by
//! rollout path, so the rollout path is never used as the resume locator.
//!
//! ## Activity
//!
//! Optional positive-evidence only: a live Codex process holding an open file
//! descriptor for the exact rollout yields possible/Active with PID/TTY.
//! Absence of such evidence is [`ActivityStatus::Unknown`][crate::session::ActivityStatus::Unknown],
//! never Inactive. This step does not depend on Step 8's SQLite enrichment.
//!
//! [resume]: crate::session::ResumeSpec

use std::{
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
};

use serde_json::Value;

use crate::{
    jsonl::{self, Bounds, FileOutcome, ReadResult},
    message::{self, Attachment, UserMessage},
    session::{
        ActivityStatus, IntegrationError, ResumeSpec, RiskStatus, Session, SessionKey,
        SupportStatus, WorkspaceEvidence,
    },
    summary,
};

/// The Codex agent name, used as the `SessionKey.agent` field.
pub const AGENT: &str = "codex";

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

/// The `type` field value of the session-meta header record.
const TYPE_SESSION_META: &str = "session_meta";

/// Default maximum number of user messages retained per session for summary
/// and preview. Bounded to keep allocation predictable.
const MAX_USER_MESSAGES: usize = 1024;

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
fn dirs_home() -> Option<PathBuf> {
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

/// Discover all Codex Sessions beneath the effective root.
///
/// Reads only rollout JSONL files. Does not touch SQLite or legacy indexes.
/// Each per-file error is isolated: a malformed rollout produces a
/// [`DiscoveredSession::Error`] entry rather than aborting discovery.
/// File bytes, mtimes, and directory entries are never modified.
pub fn discover(effective_root: &Path, bounds: &Bounds) -> Vec<DiscoveredSession> {
    discover_with_filter(effective_root, bounds, |_| true)
}

/// Discover Codex Sessions, applying a workspace filter.
///
/// `filter` receives each candidate's parsed session before construction and
/// may reject it (return `false`) — typically a Scope membership test. This
/// avoids building Session objects for out-of-scope rollouts while still
/// isolating per-file errors.
pub fn discover_with_filter<F>(
    effective_root: &Path,
    bounds: &Bounds,
    filter: F,
) -> Vec<DiscoveredSession>
where
    F: Fn(&ParsedSession) -> bool,
{
    // Canonicalize the effective root for identity stability. Fall back to
    // the provided path if it does not resolve (e.g. a test root that is
    // gone by the time we build identity).
    let canonical_root = effective_root
        .canonicalize()
        .unwrap_or_else(|_| effective_root.to_path_buf());
    let mut out = Vec::new();
    for root in rollout_roots(effective_root) {
        for path in list_rollout_files(&root.path) {
            match parse_rollout_file(&path, bounds) {
                Ok(parsed_opt) => match parsed_opt {
                    None => {}
                    Some(mut parsed) => {
                        parsed.effective_root = Some(canonical_root.clone());
                        parsed.archived = root.kind == RolloutKind::Archived;
                        if filter(&parsed) {
                            out.push(DiscoveredSession::Session(build_session(parsed)));
                        }
                    }
                },
                Err(error) => out.push(DiscoveredSession::Error { path, error }),
            }
        }
    }
    out
}

/// A discovery outcome for one rollout file.
#[derive(Debug)]
pub enum DiscoveredSession {
    /// A successfully discovered, in-scope Session.
    Session(Session),
    /// A rollout file that could not be parsed; isolated, never aborts.
    Error {
        path: PathBuf,
        error: IntegrationError,
    },
}

impl DiscoveredSession {
    /// Returns the inner Session if this is the `Session` variant.
    pub fn session(&self) -> Option<&Session> {
        match self {
            DiscoveredSession::Session(session) => Some(session),
            DiscoveredSession::Error { .. } => None,
        }
    }

    /// Iterator-like accessor over a list of outcomes yielding only Sessions.
    pub fn sessions_of(list: &[DiscoveredSession]) -> impl Iterator<Item = &Session> {
        list.iter().filter_map(DiscoveredSession::session)
    }
}

/// Recursively list rollout JSONL files beneath a scan root, sorted for
/// deterministic order. Non-`.jsonl` files (the SQLite DB, indexes, config)
/// are ignored. Symlinks to directories are not followed; symlinks to files
/// are included and confined by the reader's root guard.
fn list_rollout_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    list_rollout_files_into(root, &mut files);
    files.sort();
    files
}

fn list_rollout_files_into(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let file_type = match entry.file_type() {
            Ok(ft) => ft,
            Err(_) => continue,
        };
        let path = entry.path();
        if file_type.is_dir() {
            // Do not follow symlinked directories.
            if !entry.path().is_symlink() {
                list_rollout_files_into(&path, out);
            }
        } else if file_type.is_file() || file_type.is_symlink() {
            // Keep only rollout-*.jsonl files; ignore SQLite, indexes, config.
            if is_rollout_filename(path.file_name()) {
                out.push(path);
            }
        }
    }
}

/// True if the filename looks like a rollout JSONL file (`rollout-*.jsonl`).
pub(crate) fn is_rollout_filename(name: Option<&std::ffi::OsStr>) -> bool {
    let Some(name) = name.and_then(|n| n.to_str()) else {
        return false;
    };
    name.starts_with(ROLLOUT_PREFIX) && name.ends_with(ROLLOUT_SUFFIX)
}

/// A parsed but not-yet-constructed session: the authoritative header fields
/// plus extracted user messages and badges.
#[derive(Clone, Debug)]
pub struct ParsedSession {
    /// Canonical absolute path of the rollout file (after symlink resolution).
    pub rollout_path: PathBuf,
    /// The effective `CODEX_HOME` this rollout was discovered under. Set by
    /// the discovery flow; part of Session identity.
    pub effective_root: Option<PathBuf>,
    /// `session_meta.payload.id` — the stable identity.
    pub id: String,
    /// `session_meta.payload.cwd` — the authoritative Workspace.
    pub cwd: Option<PathBuf>,
    /// `session_meta.payload.timestamp` (ISO 8601 string as recorded).
    pub timestamp: Option<String>,
    /// `session_meta.payload.cli_version`, if present.
    pub cli_version: Option<String>,
    /// `session_meta.payload.originator`, if present.
    pub originator: Option<String>,
    /// `session_meta.payload.source`, if present (e.g. "interactive").
    pub source: Option<String>,
    /// `session_meta.payload.thread_source`, if present.
    pub thread_source: Option<String>,
    /// `session_meta.payload.parent_thread_id`, if present.
    pub parent_thread_id: Option<String>,
    /// `session_meta.payload.model_provider`, if present.
    pub model_provider: Option<String>,
    /// Whether this rollout was found in the archived root.
    pub archived: bool,
    /// Extracted, deduplicated user messages in transcript order.
    pub user_messages: Vec<UserMessage>,
    /// The JSONL read outcome (for diagnostics).
    pub outcome: FileOutcome,
    /// Count of malformed middle records.
    pub malformed_middle: usize,
    /// Parsed import metadata, if any (`foreign_session_import` equivalent).
    pub import: Option<ImportMeta>,
}

/// Safe import metadata badge. Source path/remote are never rendered by
/// default; only a coarse origin kind is exposed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImportMeta {
    /// Coarse origin kind, e.g. "codex", "claude", "omp". Display-safe.
    pub source_kind: Option<String>,
}

/// Parse a single rollout JSONL file into an optional [`ParsedSession`].
///
/// Returns `Ok(None)` when the file contains no recognizable `session_meta`
/// header (e.g. a noninteractive/transcript-only file with no session). In that
/// case the caller treats it as a non-discoverable file, not an error.
pub fn parse_rollout_file(
    path: &Path,
    bounds: &Bounds,
) -> Result<Option<ParsedSession>, IntegrationError> {
    let read = jsonl::read_file(path, bounds).map_err(|source| IntegrationError::Io {
        diagnostic: crate::session::Diagnostic {
            category: "codex_io",
            count: 1,
            verbose_path: Some(path.to_path_buf()),
            verbose_chain: Some(source.to_string()),
        },
        source,
    })?;
    parse_rollout_records(path, &read)
}

/// Parse rollout records already read via the shared JSONL reader. Separated
/// from [`parse_rollout_file`] so tests can feed pre-read records.
pub(crate) fn parse_rollout_records(
    path: &Path,
    read: &ReadResult,
) -> Result<Option<ParsedSession>, IntegrationError> {
    // Canonicalize the rollout path for identity. If canonicalization fails
    // (file removed mid-scan), fall back to the provided path.
    let rollout_path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());

    let meta = match find_session_meta(&read.records) {
        None => return Ok(None),
        Some(meta) => meta,
    };

    let payload = meta
        .get("payload")
        .and_then(Value::as_object)
        .ok_or_else(|| invalid(path, "session_meta missing payload object"))?;

    let id = payload
        .get("id")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| invalid(path, "session_meta.payload.id missing"))?;

    let cwd = payload
        .get("cwd")
        .and_then(Value::as_str)
        .map(PathBuf::from);
    let cwd = cwd.as_deref().and_then(canonicalize_workspace);

    let user_messages = extract_user_messages(&read.records);
    let import = extract_import(&read.records);

    Ok(Some(ParsedSession {
        rollout_path,
        effective_root: None,
        id,
        cwd,
        timestamp: payload
            .get("timestamp")
            .and_then(Value::as_str)
            .map(str::to_owned),
        cli_version: payload
            .get("cli_version")
            .and_then(Value::as_str)
            .map(str::to_owned),
        originator: payload
            .get("originator")
            .and_then(Value::as_str)
            .map(str::to_owned),
        source: payload
            .get("source")
            .and_then(Value::as_str)
            .map(str::to_owned),
        thread_source: payload
            .get("thread_source")
            .and_then(Value::as_str)
            .map(str::to_owned),
        parent_thread_id: payload
            .get("parent_thread_id")
            .and_then(Value::as_str)
            .map(str::to_owned),
        model_provider: payload
            .get("model_provider")
            .and_then(Value::as_str)
            .map(str::to_owned),
        archived: false,
        user_messages,
        outcome: read.outcome.clone(),
        malformed_middle: read.malformed_middle,
        import,
    }))
}

/// Find the first `session_meta` record. Unknown record types before it are
/// tolerated; a file with no `session_meta` yields `None`.
fn find_session_meta(records: &[Value]) -> Option<&Value> {
    records.iter().find(|record| {
        record
            .get("type")
            .and_then(Value::as_str)
            .is_some_and(|t| t == TYPE_SESSION_META)
    })
}

/// Build a [`Session`] from a parsed rollout, filling identity, Workspace,
/// support, activity, and risk.
///
/// `archived`/root provenance must be applied by the caller via the scan root
/// before construction (see [`discover_with_filter`], which sets `archived`
/// on the [`ParsedSession`] before filtering). This builder reads `parsed` as
/// authoritative.
pub fn build_session(parsed: ParsedSession) -> Session {
    let effective_root = parsed.effective_root.clone().unwrap_or_default();

    let native_locator = native_locator(&parsed.id, &parsed.rollout_path);
    let key = SessionKey {
        agent: OsString::from(AGENT),
        effective_root,
        profile: None,
        native_locator,
    };

    let workspace = match &parsed.cwd {
        Some(cwd) => WorkspaceEvidence::Recorded {
            workspace: cwd.clone(),
            historical_git_identity: None,
        },
        None => WorkspaceEvidence::Unknown,
    };

    let title = derive_title(&parsed);

    Session {
        key,
        resumable_id: OsString::from(parsed.id.clone()),
        title,
        workspace,
        support: SupportStatus::Supported,
        activity: ActivityStatus::Unknown,
        risk: RiskStatus::Normal,
    }
}

/// Construct the native locator for the SessionKey: the rollout UUID plus the
/// canonical rollout path, so that identity distinguishes the same native ID
/// found via a different rollout (e.g. imported into a new file).
fn native_locator(id: &str, rollout_path: &Path) -> OsString {
    let mut locator = OsString::from(id);
    locator.push("::");
    locator.push(rollout_path.as_os_str());
    locator
}

/// Canonicalize a recorded workspace if it exists; otherwise drop it to avoid
/// building a false Workspace from a directory that moved. The Session becomes
/// Unavailable rather than resuming into a wrong directory.
fn canonicalize_workspace(cwd: &Path) -> Option<PathBuf> {
    // Preserve the recorded path verbatim if it does not canonicalize (a
    // missing workspace is surfaced via WorkspaceEvidence, not dropped). We
    // only canonicalize for identity stability when it exists.
    cwd.canonicalize().ok().or_else(|| {
        if cwd.is_absolute() {
            Some(cwd.to_path_buf())
        } else {
            None
        }
    })
}

/// Derive a display title from a parsed session. Codex rollouts do not embed
/// a reliable AI title in the JSONL (titles live in the optional SQLite, which
/// this step must not depend on), so the title is a deterministic summary of
/// the first real user message.
fn derive_title(parsed: &ParsedSession) -> Option<String> {
    summary::summarize(
        parsed
            .user_messages
            .iter()
            .map(|m| m.text.clone())
            .filter(|t| !t.is_empty()),
    )
}

/// Extract and deduplicate user messages from rollout records.
///
/// Two representations are merged:
/// 1. `event_msg` records with `payload.type = "user_message"` whose payload
///    has a `message` or `payload.message` field.
/// 2. `response_item` records whose embedded message has `role = "user"`.
///
/// Deduplication is content-based: the same normalized text (with its
/// attachment fingerprint) is only retained once. Developer/system injections
/// and environmental-context records are excluded.
pub fn extract_user_messages(records: &[Value]) -> Vec<UserMessage> {
    let mut messages = Vec::new();
    let mut seen: Vec<String> = Vec::new();

    for record in records {
        let record_type = record.get("type").and_then(Value::as_str);
        match record_type {
            Some("event_msg") => {
                if let Some(msg) = extract_from_event_msg(record) {
                    push_dedup(&mut messages, &mut seen, msg);
                }
            }
            Some("response_item") => {
                if let Some(msg) = extract_from_response_item(record) {
                    push_dedup(&mut messages, &mut seen, msg);
                }
            }
            _ => {}
        }
        if messages.len() >= MAX_USER_MESSAGES {
            break;
        }
    }
    messages
}

/// Extract a user message from an `event_msg` record, when it is a
/// `user_message` payload. Returns `None` for developer/system/environmental
/// payloads.
fn extract_from_event_msg(record: &Value) -> Option<UserMessage> {
    let payload = record.get("payload")?;
    let payload_type = payload.get("type").and_then(Value::as_str)?;
    if payload_type != "user_message" {
        return None;
    }
    // The message may live at payload.message or directly in payload.
    let message = payload.get("message").unwrap_or(payload);
    extract_user_message_value(message)
}

/// Extract a user message from a `response_item` record, when its embedded
/// message has role "user". Developer/system/assistant items are excluded.
fn extract_from_response_item(record: &Value) -> Option<UserMessage> {
    let payload = record.get("payload")?;
    // response_item payload.type is typically "message" or "function_call".
    let payload_type = payload.get("type").and_then(Value::as_str);
    let message = payload
        .get("message")
        .or_else(|| payload.get("content").filter(|v| v.is_object()))
        .or_else(|| payload.get("raw_item").and_then(|r| r.get("message")))?;
    let role = message
        .get("role")
        .and_then(Value::as_str)
        .or_else(|| payload.get("role").and_then(Value::as_str));
    if role != Some("user") {
        return None;
    }
    // Only message-type response items qualify; skip function_call etc.
    if let Some(pt) = payload_type
        && pt != "message"
    {
        return None;
    }
    extract_user_message_value(message)
}

/// Extract a normalized user message from a heterogeneous message JSON value,
/// handling string content, typed content blocks, and attachment placeholders.
fn extract_user_message_value(message: &Value) -> Option<UserMessage> {
    // The message content may be a string or an array of typed blocks
    // (Codex uses { type: "input_text", text } and input_image/file).
    let content = message.get("content").or(message.get("text"));
    let (text, attachments) = match content {
        None => (None, Vec::new()),
        Some(value) => extract_codex_content(value),
    };

    // Exclude developer/system injected content: if the message explicitly
    // marks itself as such, drop it entirely.
    let role = message.get("role").and_then(Value::as_str);
    if matches!(role, Some("developer") | Some("system")) {
        return None;
    }

    Some(message::build_user_message(text, attachments))
}

/// Codex-specific content extraction that understands `input_text`,
/// `input_image`, and `input_file` block types used by rollouts, in addition
/// to the generic extraction in [`message::extract_content`].
fn extract_codex_content(value: &Value) -> (Option<String>, Vec<Attachment>) {
    match value {
        Value::String(s) => (Some(s.clone()), Vec::new()),
        Value::Array(blocks) => {
            let mut text_parts = Vec::new();
            let mut attachments = Vec::new();
            for block in blocks {
                if let Some(obj) = block.as_object() {
                    if let Some(t) = obj
                        .get("text")
                        .or_else(|| obj.get("content"))
                        .and_then(Value::as_str)
                    {
                        text_parts.push(t.to_string());
                    }
                    let kind = obj.get("type").and_then(Value::as_str);
                    match kind {
                        Some("input_image") | Some("image") => {
                            let media_type = obj
                                .get("media_type")
                                .or_else(|| obj.get("mime_type"))
                                .and_then(Value::as_str)
                                .map(String::from);
                            attachments.push(Attachment::image(media_type));
                        }
                        Some("input_file") | Some("file") => {
                            let filename = obj
                                .get("filename")
                                .or_else(|| obj.get("name"))
                                .and_then(Value::as_str)
                                .map(String::from);
                            attachments.push(Attachment::file(filename));
                        }
                        _ => {}
                    }
                }
            }
            let text = if text_parts.is_empty() {
                None
            } else {
                Some(text_parts.join("\n"))
            };
            (text, attachments)
        }
        // Nested object content (e.g. { text: "..." }).
        Value::Object(obj) => {
            if let Some(t) = obj.get("text").and_then(Value::as_str) {
                (Some(t.to_string()), Vec::new())
            } else {
                (None, Vec::new())
            }
        }
        _ => (None, Vec::new()),
    }
}

/// Push a message, skipping duplicates by a normalized content fingerprint.
fn push_dedup(messages: &mut Vec<UserMessage>, seen: &mut Vec<String>, msg: UserMessage) {
    if msg.text.trim().is_empty() && msg.attachments.is_empty() {
        return;
    }
    let fingerprint = fingerprint(&msg);
    if seen.iter().any(|s| s == &fingerprint) {
        return;
    }
    seen.push(fingerprint);
    messages.push(msg);
}

/// A stable content fingerprint for deduplication: normalized text plus
/// attachment kinds. Base64 is never part of the fingerprint.
fn fingerprint(msg: &UserMessage) -> String {
    let mut out = msg.text.trim().to_string();
    for attachment in &msg.attachments {
        out.push('|');
        match attachment {
            Attachment::Image { media_type, .. } => {
                out.push_str("image:");
                out.push_str(media_type.as_deref().unwrap_or(""));
            }
            Attachment::File { filename, .. } => {
                out.push_str("file:");
                out.push_str(filename.as_deref().unwrap_or(""));
            }
            Attachment::Text { content } => {
                out.push_str("text:");
                out.push_str(content);
            }
        }
    }
    out
}

/// Extract import metadata, if present. Codex records a `thread_source` or
/// equivalent marker when a session continues from another thread. We expose
/// only a coarse source-kind badge; the origin path/remote is never surfaced.
fn extract_import(records: &[Value]) -> Option<ImportMeta> {
    for record in records {
        let payload = record.get("payload")?;
        if let Some(import) = payload
            .get("foreign_session_import")
            .and_then(Value::as_object)
        {
            let source_kind = import
                .get("source_kind")
                .or_else(|| import.get("kind"))
                .and_then(Value::as_str)
                .map(str::to_owned);
            return Some(ImportMeta { source_kind });
        }
    }
    None
}

/// Build the ResumeSpec for a Codex session.
///
/// Program is `codex`; argv is `-C <workspace> resume <uuid>`. The cwd is the
/// recorded Workspace. A nondefault `CODEX_HOME` is preserved as an
/// environment override so resume targets the same data root discovery used.
///
/// The override is derived from the session's provenance
/// (`key.effective_root`), not a fresh env read, so resume always targets the
/// same root that discovered the session — even if the env changes between
/// discovery and resume.
pub fn resume_spec(session: &Session, default_home: &Path) -> ResumeSpec {
    let workspace = session
        .workspace
        .workspace()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| default_home.to_path_buf());

    let argv = vec![
        OsString::from("-C"),
        workspace.clone().into_os_string(),
        OsString::from("resume"),
        session.resumable_id.clone(),
    ];

    let mut env = Vec::new();
    let discovered_root = &session.key.effective_root;
    if !discovered_root.as_os_str().is_empty() && !is_default_root(discovered_root, default_home) {
        env.push((
            OsString::from(ENV_CODEX_HOME),
            discovered_root.clone().into_os_string(),
        ));
    }

    ResumeSpec {
        program: OsString::from(AGENT),
        argv,
        cwd: workspace,
        env,
    }
}

/// True if `root` resolves to the default `~/.codex`-style root (i.e. no
/// explicit `CODEX_HOME` was applied). Used to decide whether to inject the
/// environment override on resume.
fn is_default_root(root: &Path, default_home: &Path) -> bool {
    let root_canon = root.canonicalize().ok();
    let default_canon = default_home.canonicalize().ok();
    match (root_canon, default_canon) {
        (Some(a), Some(b)) => a == b,
        _ => root == default_home,
    }
}

/// Construct an [`IntegrationError::InvalidSession`] with a category and chain.
fn invalid(path: &Path, chain: &str) -> IntegrationError {
    IntegrationError::InvalidSession {
        diagnostic: crate::session::Diagnostic {
            category: "codex_invalid_session",
            count: 1,
            verbose_path: Some(path.to_path_buf()),
            verbose_chain: Some(chain.to_string()),
        },
    }
}

#[cfg(test)]
mod tests;
