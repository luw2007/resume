use std::{
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
};

use serde_json::Value;

use crate::{
    preview::jsonl::{self, Bounds, FileOutcome, ReadResult},
    preview::message::{self, Attachment, UserMessage},
    preview::summary,
    session::{
        ActivityStatus, IntegrationError, RiskStatus, Session, SessionKey, SupportStatus,
        UpdateTime, UpdateTimeSource, WorkspaceEvidence,
    },
};

use super::{AGENT, RolloutKind, cache, rollout_roots, roots::is_rollout_filename, sqlite};

/// Bounded worker count for parallel per-file discovery. Codex's per-file
/// scan is I/O-bound (real wall time far exceeds CPU time: measured on a
/// real corpus, `user 0.2s / sys 0.4s` against `real 2.1s` for one
/// single-threaded pass) and, unlike Pi/OMP/Claude's directory-pruned
/// scans, has no upper bound tied to Scope size -- a large real corpus
/// (3546 rollouts / 2.7GB) can take 18+ seconds single-threaded even after
/// the early Workspace-gate optimization. Measured in-process (no
/// per-file process spawn, which only measures process-creation overhead)
/// on that corpus: reading 1200 files serially took 1.2s; an 8-worker
/// thread pool took 0.086s (14x). 16 workers measured only marginally
/// faster (0.074s) for twice the threads spawned -- diminishing returns
/// once disk queue depth saturates. This is a scoped, documented exception
/// to "one discovery worker per integration... scans sequentially"
/// (docs/product-design.md) for Codex specifically.
const MAX_DISCOVERY_WORKERS: usize = 8;

/// Below this file count, thread-spawn overhead is not worth paying --
/// the large majority of real Scopes have far fewer Codex rollouts.
const PARALLEL_THRESHOLD: usize = 16;

/// Applies `f` to every element of `items` using up to
/// [`MAX_DISCOVERY_WORKERS`] scoped threads, preserving `items`' original
/// order in the result: each worker owns one contiguous chunk, and chunk
/// outputs are concatenated in chunk order. Falls back to a plain
/// sequential map below [`PARALLEL_THRESHOLD`] items.
fn parallel_map<T, R>(items: &[T], f: impl Fn(&T) -> R + Sync) -> Vec<R>
where
    T: Sync,
    R: Send,
{
    if items.len() < PARALLEL_THRESHOLD {
        return items.iter().map(&f).collect();
    }
    let workers = MAX_DISCOVERY_WORKERS.min(items.len());
    let chunk_size = items.len().div_ceil(workers);
    std::thread::scope(|scope| {
        items
            .chunks(chunk_size)
            .map(|chunk| scope.spawn(|| chunk.iter().map(&f).collect::<Vec<R>>()))
            .collect::<Vec<_>>()
            .into_iter()
            .flat_map(|handle| handle.join().expect("discovery worker thread panicked"))
            .collect()
    })
}

/// The `type` field value of the session-meta header record.
const TYPE_SESSION_META: &str = "session_meta";

/// Default maximum number of user messages retained per session for summary
/// and preview. Bounded to keep allocation predictable.
const MAX_USER_MESSAGES: usize = 1024;

/// Discover all Codex Sessions beneath the effective root.
///
/// Reads only rollout JSONL files. Does not touch SQLite or legacy indexes.
/// Each per-file error is isolated: a malformed rollout produces a
/// [`DiscoveredSession::Error`] entry rather than aborting discovery.
/// File bytes, mtimes, and directory entries are never modified.
pub fn discover(effective_root: &Path, bounds: &Bounds) -> Vec<DiscoveredSession> {
    discover_with_filter(effective_root, bounds, None, |_| true)
}

/// An early per-file Workspace gate: receives `session_meta.payload.cwd`
/// resolved through the same `resolve_workspace_cwd` rule as the full parse
/// (so it sees exactly the value the post-parse filter would see as
/// `parsed.cwd`) and returns whether the rollout could be in Scope. `None`
/// disables the gate (every file is fully parsed).
///
/// This is a pure read-cost optimization: when the gate rejects a `cwd`, the
/// rollout is skipped after a small first-record read instead of paying the
/// full bounded read and per-line parse for title derivation. Callers MUST
/// pass a gate consistent with their post-parse `filter` (typically the same
/// `Scope::contains_workspace` check); the post-parse filter remains
/// authoritative for every session actually parsed. Files whose
/// `session_meta` has no `cwd`, or a relative/unresolvable one (which the
/// full parse drops to `parsed.cwd == None` -- kept unconditionally by the
/// post-parse filter), are never offered to the gate.
pub type WorkspaceGate<'a> = &'a (dyn Fn(&Path) -> bool + Sync);

/// Discover Codex Sessions, applying a workspace filter.
///
/// `filter` receives each candidate's parsed session before construction and
/// may reject it (return `false`) — typically a Scope membership test. This
/// avoids building Session objects for out-of-scope rollouts while still
/// isolating per-file errors.
pub fn discover_with_filter<F>(
    effective_root: &Path,
    bounds: &Bounds,
    workspace_gate: Option<WorkspaceGate<'_>>,
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
        if let Some((path, error)) = unreadable_root(&root.path) {
            out.push(DiscoveredSession::Error { path, error });
            continue;
        }
        for path in list_rollout_files(&root.path) {
            match parse_rollout_file_gated(&path, &canonical_root, bounds, workspace_gate) {
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

/// Discover Codex Sessions with optional `state_5.sqlite` enrichment (Step 8).
///
/// This is the enriched variant of [`discover_with_filter`]. It performs the
/// **exact same** JSONL-based discovery — the same rollout files are parsed,
/// the same identity/Workspace rules apply — and *then* optionally consults
/// `state_5.sqlite` to enrich titles, activity times, and archived hints for
/// sessions that have no JSONL-derived title.
///
/// **Strictly additive and optional.** The `sqlite::SqliteOutcome` reports
/// whether enrichment happened; when it is `Absent` or `Degraded`, the
/// returned [`DiscoveredSession`] list is byte-for-byte identical to what
/// [`discover_with_filter`] would have produced. Deleting the DB never changes
/// which sessions are discoverable or resumable.
///
/// The JSONL rollout remains authoritative: identity and Workspace come only
/// from `session_meta`, and the DB may only fill in missing presentation
/// fields. Disagreement produces a diagnostic in the outcome rather than
/// replacing identity.
pub fn discover_with_filter_enriched<F>(
    effective_root: &Path,
    bounds: &Bounds,
    workspace_gate: Option<WorkspaceGate<'_>>,
    filter: F,
    cache: Option<&cache::DiscoveryCache>,
) -> (Vec<DiscoveredSession>, sqlite::SqliteOutcome)
where
    F: Fn(&ParsedSession) -> bool + Sync,
{
    let canonical_root = effective_root
        .canonicalize()
        .unwrap_or_else(|_| effective_root.to_path_buf());

    // Parse every rollout first, in the exact same order as the JSONL-only
    // path (list_rollout_files is already sorted). We hold the parsed sessions
    // (filtered) so enrichment can attach DB hints before build_session turns
    // them into the immutable Session type. Errors are emitted in file order,
    // interleaved with sessions, exactly like discover_with_filter.
    enum Pending {
        Parsed {
            rollout_path: PathBuf,
            slot: Option<Box<ParsedSession>>,
        },
        Error {
            path: PathBuf,
            error: IntegrationError,
        },
    }
    let mut pending: Vec<Pending> = Vec::new();
    // Every rollout path actually observed under `effective_root` this run
    // (regardless of parse outcome) -- only meaningful when caching is
    // active, since only then does `parse_for_discovery` bypass the
    // Workspace gate and guarantee every current file gets visited. Used to
    // prune the cache of entries for files deleted since they were cached
    // (see `cache::DiscoveryCache::save`).
    let mut seen: Vec<PathBuf> = Vec::new();
    for root in rollout_roots(effective_root) {
        if let Some((path, error)) = unreadable_root(&root.path) {
            pending.push(Pending::Error { path, error });
            continue;
        }
        let paths = list_rollout_files(&root.path);
        let results = parallel_map(&paths, |path| {
            parse_for_discovery(path, &canonical_root, bounds, workspace_gate, cache)
        });
        for (path, (canonical_path, result)) in paths.into_iter().zip(results) {
            if cache.is_some() {
                seen.push(canonical_path);
            }
            match result {
                Ok(None) => {}
                Ok(Some(mut parsed)) => {
                    parsed.effective_root = Some(canonical_root.clone());
                    parsed.archived = root.kind == RolloutKind::Archived;
                    if filter(&parsed) {
                        let rollout_path = parsed.rollout_path.clone();
                        pending.push(Pending::Parsed {
                            rollout_path,
                            slot: Some(Box::new(parsed)),
                        });
                    }
                }
                Err(error) => pending.push(Pending::Error { path, error }),
            }
        }
    }
    if let Some(cache) = cache {
        cache.save(&canonical_root, &seen);
    }

    // Collect the parsed sessions for enrichment.
    let mut kept: Vec<ParsedSession> = pending
        .iter_mut()
        .filter_map(|p| match p {
            Pending::Parsed { slot, .. } => slot.take().map(|b| *b),
            Pending::Error { .. } => None,
        })
        .collect();

    // Enrich in place. Never raises; never changes identity or Workspace.
    let outcome = sqlite::enrich(&mut kept, effective_root);

    // Re-assemble in the original order, building immutable Sessions from
    // the (possibly enriched) parsed data. We match parsed sessions back to
    // their pending slot by rollout path, which is unique per session.
    let mut kept_by_path: std::collections::HashMap<PathBuf, ParsedSession> = kept
        .into_iter()
        .map(|ps| (ps.rollout_path.clone(), ps))
        .collect();
    let out: Vec<DiscoveredSession> = pending
        .into_iter()
        .map(|p| match p {
            Pending::Parsed { rollout_path, .. } => {
                let enriched = kept_by_path
                    .remove(&rollout_path)
                    .expect("enriched session must map back to its pending slot");
                DiscoveredSession::Session(build_session(enriched))
            }
            Pending::Error { path, error } => DiscoveredSession::Error { path, error },
        })
        .collect();
    (out, outcome)
}

/// Parse one rollout file for discovery, consulting `cache` first and
/// recording the outcome for later write-back on a miss. Returns the
/// canonical (symlink-resolved) path alongside the outcome so the caller
/// can track which paths were actually seen this run for cache pruning --
/// only computed when a cache is present (the no-cache path returns the
/// original, non-canonical `path`, which no caller relies on).
///
/// Skips [`WorkspaceGate`] entirely whenever a cache is present: caching
/// requires the file's TRUE content regardless of Scope (see the `cache`
/// module doc -- a gate rejection is Scope-specific and must never be
/// cached as "no session"), so a cache miss always does the full, ungated
/// parse. This also makes a cached entry reusable by a *different* Scope's
/// later invocation, not just a rerun of this exact one -- a real benefit
/// given `resume` is typically invoked from many different project
/// directories over time against the same underlying rollout store.
/// Cache keys and the reconstructed `rollout_path` both use the symlink-
/// resolved canonical path, matching what a fresh parse produces.
fn parse_for_discovery(
    path: &Path,
    effective_root: &Path,
    bounds: &Bounds,
    workspace_gate: Option<WorkspaceGate<'_>>,
    cache: Option<&cache::DiscoveryCache>,
) -> (PathBuf, Result<Option<ParsedSession>, IntegrationError>) {
    let Some(cache) = cache else {
        return (
            path.to_path_buf(),
            parse_rollout_file_gated(path, effective_root, bounds, workspace_gate),
        );
    };
    let canonical_path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    if let Some(hit) = cache.lookup(&canonical_path) {
        return (canonical_path, Ok(hit));
    }
    let result = parse_rollout_file(path, effective_root, bounds);
    if let Ok(parsed) = &result
        && let Ok(metadata) = fs::metadata(&canonical_path)
        && let Ok(mtime) = metadata.modified()
    {
        cache.record(&canonical_path, metadata.len(), mtime, parsed.as_ref());
    }
    (canonical_path, result)
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
fn unreadable_root(root: &Path) -> Option<(PathBuf, IntegrationError)> {
    match fs::read_dir(root) {
        Ok(_) => None,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(source) => {
            let path = root.to_path_buf();
            Some((
                path.clone(),
                IntegrationError::Io {
                    diagnostic: crate::session::Diagnostic {
                        category: "codex_root_unavailable",
                        count: 1,
                        verbose_path: Some(path),
                        verbose_chain: Some(source.to_string()),
                    },
                    source,
                },
            ))
        }
    }
}

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
    /// Optional title hint sourced from `state_5.sqlite` (Step 8 enrichment).
    /// Only set when the JSONL has no usable user message to derive a title
    /// from, and only after the DB row passed identity/Workspace precedence
    /// checks. Never overrides a JSONL-derived identity. `None` when the
    /// `codex-sqlite` feature is off, the DB is absent, or no row matched.
    pub sqlite_title: Option<String>,
    /// Optional activity-time hint sourced from `state_5.sqlite`.
    /// Additive metadata only; never used as identity or to mark a session
    /// Inactive.
    pub sqlite_activity_time: Option<std::time::SystemTime>,
    /// Optional archived hint sourced from `state_5.sqlite` (Step 8). May not
    /// override a filesystem-derived `archived` value. Informational only.
    pub sqlite_archived_hint: Option<bool>,
}

/// Safe import metadata badge. Source path/remote are never rendered by
/// default; only a coarse origin kind is exposed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImportMeta {
    /// Coarse origin kind, e.g. "codex", "claude", "omp". Display-safe.
    pub source_kind: Option<String>,
}

impl ImportMeta {
    /// A safe display string for the badge. Never exposes an origin path or
    /// remote — only the coarse source kind, when known.
    pub fn to_display(&self) -> String {
        match &self.source_kind {
            Some(kind) => format!("imported from {kind}"),
            None => "imported".to_string(),
        }
    }
}

/// Discovery-time early-read budget, per `docs/product-design.md` section 3:
/// "at most a 1 MiB bounded early read" for title derivation -- 64 KiB is
/// well within that documented ceiling, not a separate mechanism. Real
/// `session_meta` headers are near the start of the file (see
/// `docs/research/session-formats.md`: "The normal first record is
/// `session_meta`"), and title derivation only needs the *first* non-empty
/// user message (`summary::summarize_texts` returns on the first match).
///
/// Chosen from real-corpus measurement (3546 real rollouts, ~2.9 GB): a
/// 64 KiB budget finds `session_meta` in exactly the same set of files as a
/// 1 MiB budget did (0 additional fallbacks triggered), and produces a
/// byte-identical first-user-message title for all 3580 discoverable
/// sessions when compared against the full 1 MiB read. Reading 1 MiB per
/// file when 64 KiB already suffices for every real file observed was pure
/// waste: shrinking the bound cut steady-state Codex discovery time on that
/// corpus from ~5.5s to ~1.8s median (see PERFORMANCE.md). Large outlier
/// rollouts (tens of MB, common after long-lived real usage) previously had
/// their *entire* content read and every line JSON-parsed during discovery
/// purely to find this header and one message; this bound turns that into
/// O(64 KiB) for the common case instead of O(file size), and falls back to
/// a full read (see below) only for the rare file where 64 KiB is not
/// enough, so correctness never trades off against speed.
const DISCOVERY_EARLY_READ_BYTES: u64 = 64 * 1024;
/// Workspace-gate read: stop after the FIRST parsed record (`max_records: 1`
/// -- the shared reader stops reading as soon as one record parses), inside
/// the same byte budget as the early read. Real `session_meta` first lines
/// are large (median ~4 KiB, p99 ~22 KiB on a real 3582-file corpus: the
/// payload embeds instructions), so a small fixed byte budget would truncate
/// most of them and defeat the gate; the record cap makes the gate cost
/// "read to the first newline + one parse" regardless of line size. A first
/// record that is not `session_meta`, is malformed, or exceeds the byte
/// budget simply falls through to the normal ladder below -- the gate never
/// trades correctness, only skips work for files it can already rule out.
///
/// [`parse_rollout_file`] with an optional early Workspace gate (see
/// [`WorkspaceGate`]).
///
/// With a gate, a first-record read resolves `session_meta.payload.cwd`; a
/// rejected `cwd` returns `Ok(None)` without the title-derivation read. The
/// gate read is never used as a parse source: an accepted (or `cwd`-less)
/// rollout continues through the exact same read ladder as the ungated
/// path, so titles and every other field are byte-identical with and
/// without a gate.
pub fn parse_rollout_file_gated(
    path: &Path,
    effective_root: &Path,
    bounds: &Bounds,
    workspace_gate: Option<WorkspaceGate<'_>>,
) -> Result<Option<ParsedSession>, IntegrationError> {
    if let Some(gate) = workspace_gate {
        let gate_bounds = Bounds {
            max_file_bytes: bounds.max_file_bytes.min(DISCOVERY_EARLY_READ_BYTES),
            max_records: 1,
            ..bounds.clone()
        };
        let gate_read = read_confined(path, effective_root, &gate_bounds)?;
        if let Some(meta) = find_session_meta(&gate_read.records)
            && let Some(cwd) = meta
                .get("payload")
                .and_then(Value::as_object)
                .and_then(|payload| payload.get("cwd"))
                .and_then(Value::as_str)
            // Resolve exactly as the full parse does (`resolve_workspace_cwd`):
            // a relative/unresolvable cwd becomes `parsed.cwd == None` there,
            // which the post-parse filter keeps unconditionally -- so the gate
            // must not reject it either.
            && let Some(resolved) = resolve_workspace_cwd(Path::new(cwd))
            && !gate(&resolved)
        {
            return Ok(None);
        }
    }
    parse_rollout_file(path, effective_root, bounds)
}

/// Parse a single rollout JSONL file into an optional [`ParsedSession`].
///
/// Returns `Ok(None)` when the file contains no recognizable `session_meta`
/// header (e.g. a noninteractive/transcript-only file with no session). In that
/// case the caller treats it as a non-discoverable file, not an error.
pub fn parse_rollout_file(
    path: &Path,
    effective_root: &Path,
    bounds: &Bounds,
) -> Result<Option<ParsedSession>, IntegrationError> {
    // Fast path: a small bounded read covers the overwhelming majority of
    // real rollouts (session_meta near the start, one early user message).
    // Only fall back to the full caller-supplied `bounds` (still capped at
    // its own safety ceiling, never unbounded) when session_meta was not
    // found within the fast-path budget -- an anomalous shape, not the
    // common case, so paying the full read cost there is acceptable.
    let early_bounds = Bounds {
        max_file_bytes: bounds.max_file_bytes.min(DISCOVERY_EARLY_READ_BYTES),
        ..bounds.clone()
    };
    let early_read = read_confined(path, effective_root, &early_bounds)?;
    if find_session_meta(&early_read.records).is_some() {
        return parse_rollout_records(path, &early_read);
    }
    let full_read = read_confined(path, effective_root, bounds)?;
    parse_rollout_records(path, &full_read)
}

fn read_confined(
    path: &Path,
    effective_root: &Path,
    bounds: &Bounds,
) -> Result<ReadResult, IntegrationError> {
    jsonl::read_file_confined(path, effective_root, bounds).map_err(|source| IntegrationError::Io {
        diagnostic: crate::session::Diagnostic {
            category: "codex_io",
            count: 1,
            verbose_path: Some(path.to_path_buf()),
            verbose_chain: Some(source.to_string()),
        },
        source,
    })
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
        None if read.malformed_middle > 0
            || matches!(read.outcome, FileOutcome::IncompleteTail) =>
        {
            return Err(invalid(
                path,
                "rollout contains malformed JSON and no recognizable session_meta",
            ));
        }
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
    let cwd = cwd.as_deref().and_then(resolve_workspace_cwd);

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
        sqlite_title: None,
        sqlite_activity_time: None,
        sqlite_archived_hint: None,
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

    let title = match (derive_title(&parsed), parsed.import.as_ref()) {
        (Some(title), Some(import)) => Some(format!("{title} [{}]", import.to_display())),
        (None, Some(import)) => Some(import.to_display()),
        (title, None) => title,
    };

    Session {
        key,
        resumable_id: OsString::from(parsed.id.clone()),
        title,
        workspace,
        updated_at: parsed
            .sqlite_activity_time
            .or_else(|| {
                parsed
                    .timestamp
                    .as_deref()
                    .and_then(crate::time::parse_iso8601)
            })
            .map(|at| UpdateTime {
                at,
                source: UpdateTimeSource::Native,
            })
            .or_else(|| {
                fs::metadata(&parsed.rollout_path)
                    .and_then(|metadata| metadata.modified())
                    .ok()
                    .map(|at| UpdateTime {
                        at,
                        source: UpdateTimeSource::FileMtime,
                    })
            }),
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

/// Recorded Workspace resolution: an absolute `cwd` is kept exactly as
/// Codex wrote it, matching every other integration's WorkspaceEvidence
/// contract (Claude/Pi/OMP never resolve symlinks in the stored workspace
/// either -- canonical resolution for Scope membership and Directory
/// Distance is `scope::canonical_workspace`'s job, not identity/display).
/// A relative `cwd` cannot be a valid recorded Workspace and is dropped so
/// the Session surfaces without one (`WorkspaceEvidence::Unknown`), rather
/// than resolving it against this process's unrelated working directory.
fn resolve_workspace_cwd(cwd: &Path) -> Option<PathBuf> {
    if cwd.is_absolute() {
        Some(cwd.to_path_buf())
    } else {
        None
    }
}

/// Derive a display title from a parsed session. Codex rollouts do not embed
/// a reliable AI title in the JSONL (titles live in the optional SQLite, which
/// this step must not depend on), so the title is a deterministic summary of
/// the first real user message. When there are no usable user messages, an
/// optional `state_5.sqlite` title hint (Step 8) may be used as a fallback —
/// but only as enrichment, never overriding a JSONL-derived title.
fn derive_title(parsed: &ParsedSession) -> Option<String> {
    let from_jsonl = summary::summarize(
        parsed
            .user_messages
            .iter()
            .map(|m| m.text.clone())
            .filter(|t| !t.is_empty()),
    );
    // JSONL is authoritative; the SQLite hint only fills a missing title.
    from_jsonl.or_else(|| parsed.sqlite_title.clone())
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
        let Some(payload) = record.get("payload") else {
            continue;
        };
        if let Some(import) = payload
            .get("foreign_session_import")
            .and_then(Value::as_object)
        {
            let source_kind = import
                .get("source_kind")
                .or_else(|| import.get("kind"))
                .and_then(Value::as_str)
                .and_then(safe_badge_token);
            return Some(ImportMeta { source_kind });
        }
    }
    None
}

/// Return a short display-safe import metadata token. Import payloads are
/// transcript data, not trusted UI input, so paths/remotes never qualify as a
/// source kind or origin identifier badge.
fn safe_badge_token(value: &str) -> Option<String> {
    let value = value.trim();
    (1..=32)
        .contains(&value.len())
        .then_some(value)
        .filter(|value| {
            value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        })
        .map(str::to_owned)
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
