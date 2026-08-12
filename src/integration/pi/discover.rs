use super::DISCOVERY_SCAN_RECORDS;
use super::roots::EffectiveRoots;
use crate::{
    preview::jsonl::{self, Bounds, ReadResult},
    preview::message::{self, UserMessage},
    scope::Scope,
};
use serde_json::Value;
use std::{
    fs, io,
    path::{Path, PathBuf},
    time::SystemTime,
};
/// Configuration for a Pi discovery pass.
#[derive(Clone, Debug)]
pub struct DiscoverConfig<'a> {
    /// Effective Pi roots (owned so callers can pass temporary values).
    pub roots: EffectiveRoots,
    /// Scope used to filter Sessions by header `cwd`.
    pub scope: &'a Scope,
    /// Bounds for the JSONL reader. Discovery uses a record cap.
    pub bounds: Bounds,
    /// Home directory for the grouped-directory-name prefilter (OMP encodes
    /// home-relative Workspace paths). Tests override; production uses `$HOME`.
    pub home: Option<PathBuf>,
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
            home: std::env::var_os("HOME").map(PathBuf::from),
        }
    }
}

/// A discovered Pi session with its parsed metadata, ready to become a
/// [`crate::session::Session`] via [`ParsedSession::into_session`]. Holding this separately
/// lets tests inspect extracted fields and the caller dedupe before building
/// final [`crate::session::Session`] values.
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
    /// Number of grouped Workspace directories pruned by the directory-name
    /// prefilter without reading any file inside them.
    pub pruned_dirs: usize,
}

/// Discover Pi sessions under the effective session root. Reads JSONL
/// read-only through the shared reader, parses v1/v2/v3 headers, and filters
/// by header `cwd` through Scope. Never invokes Pi or migrates files.
///
/// Discovery scans `.jsonl` files one level under the session root. In the
/// default grouped layout that means `<session-root>/<encoded-workspace>/*.jsonl`,
/// and directory names are used as a lossy Scope prefilter: a grouped
/// directory whose encoded name cannot correspond to any in-Scope Workspace
/// is skipped without reading its files (`Scope::may_contain_session_dir`).
/// In a custom flat layout (`custom_session_root`) directory names carry no
/// encoding and are never pruned. Header `cwd` stays authoritative for every
/// file that is read.
pub fn discover(config: &DiscoverConfig<'_>) -> io::Result<DiscoverOutcome> {
    let session_root = config.roots.session_root.clone();
    let confined_root = session_root
        .canonicalize()
        .unwrap_or_else(|_| session_root.clone());
    let mut outcome = DiscoverOutcome::default();
    let mut seen: Vec<(PathBuf, PathBuf)> = Vec::new();

    for jsonl_path in iter_session_files(config, &mut outcome)? {
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
            Some(workspace) if !config.scope.contains_workspace(workspace) => {
                outcome.out_of_scope += 1;
                continue;
            }
            Some(_) => {}
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
fn iter_session_files(
    config: &DiscoverConfig<'_>,
    outcome: &mut DiscoverOutcome,
) -> io::Result<Vec<PathBuf>> {
    let session_root = &config.roots.session_root;
    let mut paths = Vec::new();
    if !session_root.exists() {
        return Ok(paths);
    }
    collect_jsonl(config, session_root, &mut paths, outcome)?;
    // Sort for deterministic discovery order.
    paths.sort();
    Ok(paths)
}

/// Recursively collect `.jsonl` file paths over the storage layout. In the
/// default grouped layout, encoded Workspace directory names always start
/// with `-` (`-{abs path with '/' -> '-'}-`, or OMP's home-relative form);
/// such a directory whose name cannot encode any in-Scope Workspace is
/// pruned without reading it (counted in `outcome.pruned_dirs`). Any other
/// directory (e.g. a literal `sessions` level between the agent root and
/// the grouped directories) is descended unconditionally, and custom
/// session roots are never pruned.
fn collect_jsonl(
    config: &DiscoverConfig<'_>,
    dir: &Path,
    out: &mut Vec<PathBuf>,
    outcome: &mut DiscoverOutcome,
) -> io::Result<()> {
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
            if !config.roots.custom_session_root
                && let Some(name) = entry.file_name().to_str()
                && name.starts_with('-')
                && !config
                    .scope
                    .may_contain_session_dir(name, config.home.as_deref())
            {
                outcome.pruned_dirs += 1;
                continue;
            }
            collect_jsonl(config, &path, out, outcome)?;
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

#[cfg(test)]
#[path = "tests/discover.rs"]
mod tests;
