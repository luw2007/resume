//! Persistent Codex discovery cache (see `docs/product-design.md` "Codex
//! discovery cache"): reuses a prior run's [`ParsedSession`] for a rollout
//! file whose size and mtime have not changed, skipping its read and JSON
//! parse entirely on a hit.
//!
//! Purely a discovery-speed optimization, with the same posture as the
//! `state_5.sqlite` enrichment: the rollout JSONL remains authoritative, a
//! missing/corrupt/version-mismatched cache silently degrades to a full
//! fresh scan (never blocks discovery, never changes the result), and
//! deleting the cache file is always safe -- it holds no information not
//! re-derivable from the rollout store itself.
//!
//! Scope: only a file that underwent a full, *ungated* parse this run is
//! recorded. A Scope's early Workspace-gate rejection is specific to that
//! run's Scope, not a durable fact about the file, and must never be cached
//! as "no session" -- a later run with a different Scope would then wrongly
//! believe the file has no Session at all. See `discover::parse_for_cache`.
//! A parse `Err` is also never cached: errors are rare, and re-checking one
//! every run is cheap insurance against a transient condition (permissions,
//! a half-written file) healing.
//!
//! One shared file for every `CODEX_HOME`: entries are keyed by each
//! rollout's absolute path, which already encodes which root it came from,
//! so a nonstandard `CODEX_HOME` cannot collide with the default `~/.codex`.
//! `save` prunes an entry whose path falls under the *current* run's
//! effective root but was not seen this run -- the file no longer exists,
//! since caching means every current file under that root is always
//! enumerated (the Workspace gate never applies when a cache is present).
//! An entry under a *different* effective root (a different `CODEX_HOME`
//! from an earlier run) is left untouched: this run has no fresh
//! information about that root, so it is never treated as orphaned.

use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::Mutex,
    time::SystemTime,
};

use serde::{Deserialize, Serialize};

use super::ImportMeta;
use super::discover::ParsedSession;
use crate::preview::jsonl::FileOutcome;
use crate::preview::message::{Attachment, UserMessage};

/// Schema version. Bump on any incompatible entry-shape change; an old file
/// is discarded wholesale on load rather than migrated.
const CACHE_VERSION: u32 = 2;
const CACHE_FILE_NAME: &str = "codex-discovery-v1.json";

#[derive(Serialize, Deserialize, Default)]
struct CacheFile {
    version: u32,
    entries: HashMap<String, CacheEntry>,
}

#[derive(Clone, Serialize, Deserialize)]
struct CacheEntry {
    size: u64,
    mtime_unix_nanos: u128,
    outcome: CachedOutcome,
}

#[derive(Clone, Serialize, Deserialize)]
enum CachedOutcome {
    /// The rollout was fully read and had no recognizable `session_meta`.
    NoSession,
    Session(Box<CachedParsedSession>),
}

#[derive(Clone, Serialize, Deserialize)]
struct CachedParsedSession {
    id: String,
    cwd: Option<PathBuf>,
    timestamp: Option<String>,
    cli_version: Option<String>,
    originator: Option<String>,
    source: Option<String>,
    structured_source: Option<serde_json::Value>,
    thread_source: Option<String>,
    parent_thread_id: Option<String>,
    model_provider: Option<String>,
    user_messages: Vec<CachedUserMessage>,
    file_outcome: CachedFileOutcome,
    malformed_middle: usize,
    import: Option<CachedImportMeta>,
}

#[derive(Clone, Serialize, Deserialize)]
struct CachedUserMessage {
    text: String,
    attachments: Vec<CachedAttachment>,
}

#[derive(Clone, Serialize, Deserialize)]
enum CachedAttachment {
    Image { media_type: Option<String> },
    File { filename: Option<String> },
    Text { content: String },
}

#[derive(Clone, Serialize, Deserialize)]
enum CachedFileOutcome {
    Complete,
    IncompleteTail,
    BoundExceeded,
}

#[derive(Clone, Serialize, Deserialize)]
struct CachedImportMeta {
    source_kind: Option<String>,
}

/// Resolve the cache file location the same way `config::discover_path`
/// resolves the config file: explicit inputs, no direct env reads, so tests
/// (and the QA fixture harness's `XDG_CACHE_HOME`) can inject isolation.
/// `None` when neither `XDG_CACHE_HOME` nor `HOME` is available -- the
/// cache is then simply unavailable, exactly like a load/save failure.
pub fn cache_path(xdg_cache_home: Option<PathBuf>, home: Option<PathBuf>) -> Option<PathBuf> {
    if let Some(xdg) = xdg_cache_home {
        return Some(xdg.join("resume").join(CACHE_FILE_NAME));
    }
    home.map(|home| home.join(".cache/resume").join(CACHE_FILE_NAME))
}

/// Loaded once per `resume` process. Lookups read the immutable `loaded`
/// map (no lock); newly parsed entries collect into `fresh` under a single
/// mutex for one batched write-back, so parallel discovery workers never
/// contend on a per-entry basis.
pub struct DiscoveryCache {
    path: Option<PathBuf>,
    loaded: HashMap<String, CacheEntry>,
    fresh: Mutex<HashMap<String, CacheEntry>>,
}

impl DiscoveryCache {
    /// Load from `path` (see [`cache_path`]). A missing file, unreadable
    /// file, malformed JSON, or version mismatch all load as empty -- never
    /// an error, matching the non-authoritative posture above.
    pub fn load(path: Option<PathBuf>) -> Self {
        let loaded = path
            .as_deref()
            .and_then(|p| fs::read(p).ok())
            .and_then(|bytes| serde_json::from_slice::<CacheFile>(&bytes).ok())
            .filter(|file| file.version == CACHE_VERSION)
            .map(|file| file.entries)
            .unwrap_or_default();
        Self {
            path,
            loaded,
            fresh: Mutex::new(HashMap::new()),
        }
    }

    /// Returns the cached parse outcome for `key` if a recorded entry's
    /// (size, mtime) still match -- checked against both the loaded file
    /// and anything already recorded earlier in this same run (the same
    /// rollout can be visited twice across the active/archived roots in
    /// one discovery pass).
    fn get(&self, key: &str, size: u64, mtime_unix_nanos: u128) -> Option<CachedOutcome> {
        let hit = self
            .fresh
            .lock()
            .unwrap()
            .get(key)
            .cloned()
            .or_else(|| self.loaded.get(key).cloned())?;
        if hit.size == size && hit.mtime_unix_nanos == mtime_unix_nanos {
            Some(hit.outcome)
        } else {
            None
        }
    }

    /// Looks up a cached, fully reconstructed [`ParsedSession`] for
    /// `rollout_path` (`Some(None)` = cached "no session"; `None` = miss).
    /// `rollout_path` must already be canonicalized identically to how a
    /// fresh parse would produce `ParsedSession::rollout_path`.
    pub fn lookup(&self, rollout_path: &Path) -> Option<Option<ParsedSession>> {
        let metadata = fs::metadata(rollout_path).ok()?;
        let mtime = mtime_unix_nanos(&metadata)?;
        let key = cache_key(rollout_path);
        match self.get(&key, metadata.len(), mtime)? {
            CachedOutcome::NoSession => Some(None),
            CachedOutcome::Session(cached) => {
                Some(Some(from_cached(rollout_path.to_path_buf(), *cached)))
            }
        }
    }

    /// Records this run's parse outcome for `rollout_path` (from a full,
    /// ungated parse -- see the module doc). `size`/`mtime` are read once
    /// by the caller alongside the parse itself, avoiding a second stat.
    pub fn record(
        &self,
        rollout_path: &Path,
        size: u64,
        mtime: SystemTime,
        parsed: Option<&ParsedSession>,
    ) {
        let Some(mtime_unix_nanos) = duration_nanos(mtime) else {
            return;
        };
        let outcome = match parsed {
            None => CachedOutcome::NoSession,
            Some(parsed) => CachedOutcome::Session(Box::new(to_cached(parsed))),
        };
        let entry = CacheEntry {
            size,
            mtime_unix_nanos,
            outcome,
        };
        self.fresh
            .lock()
            .unwrap()
            .insert(cache_key(rollout_path), entry);
    }

    /// Writes this run's cache state to disk in one pass: every entry
    /// recorded this run, merged over whatever was loaded, then pruned of
    /// orphans -- an entry whose path is under `effective_root` but not in
    /// `seen` no longer exists on disk (deleted since it was cached) and is
    /// dropped rather than lingering forever. An entry under a *different*
    /// root is always kept: this run scanned only `effective_root`, so it
    /// has no evidence either way about anything outside it. Best-effort --
    /// a write failure (permissions, disk full) is silently swallowed;
    /// losing an update is never a correctness problem, only a missed
    /// speedup on the next run.
    pub fn save(&self, effective_root: &Path, seen: &[PathBuf]) {
        let Some(path) = &self.path else { return };
        let seen_keys: std::collections::HashSet<String> =
            seen.iter().map(|p| cache_key(p)).collect();
        let fresh = self.fresh.lock().unwrap();
        let mut entries = self.loaded.clone();
        entries.extend(fresh.iter().map(|(k, v)| (k.clone(), v.clone())));
        let before = entries.len();
        entries.retain(|key, _| {
            !Path::new(key.as_str()).starts_with(effective_root) || seen_keys.contains(key)
        });
        let pruned = before - entries.len();
        if fresh.is_empty() && pruned == 0 {
            return;
        }
        let file = CacheFile {
            version: CACHE_VERSION,
            entries,
        };
        let Ok(json) = serde_json::to_vec(&file) else {
            return;
        };
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let _ = fs::write(path, json);
    }
}

fn cache_key(rollout_path: &Path) -> String {
    rollout_path.to_string_lossy().into_owned()
}

fn mtime_unix_nanos(metadata: &fs::Metadata) -> Option<u128> {
    metadata.modified().ok().and_then(duration_nanos)
}

fn duration_nanos(time: SystemTime) -> Option<u128> {
    time.duration_since(SystemTime::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_nanos())
}

fn to_cached(parsed: &ParsedSession) -> CachedParsedSession {
    CachedParsedSession {
        id: parsed.id.clone(),
        cwd: parsed.cwd.clone(),
        timestamp: parsed.timestamp.clone(),
        cli_version: parsed.cli_version.clone(),
        originator: parsed.originator.clone(),
        source: parsed.source.clone(),
        structured_source: parsed.structured_source.clone(),
        thread_source: parsed.thread_source.clone(),
        parent_thread_id: parsed.parent_thread_id.clone(),
        model_provider: parsed.model_provider.clone(),
        user_messages: parsed.user_messages.iter().map(cache_message).collect(),
        file_outcome: cache_file_outcome(&parsed.outcome),
        malformed_middle: parsed.malformed_middle,
        import: parsed.import.as_ref().map(|import| CachedImportMeta {
            source_kind: import.source_kind.clone(),
        }),
    }
}

fn from_cached(rollout_path: PathBuf, cached: CachedParsedSession) -> ParsedSession {
    ParsedSession {
        rollout_path,
        // Always overwritten by the caller right after a successful parse
        // (see discover_with_filter/_enriched), regardless of cache hit.
        effective_root: None,
        id: cached.id,
        cwd: cached.cwd,
        timestamp: cached.timestamp,
        cli_version: cached.cli_version,
        originator: cached.originator,
        source: cached.source,
        structured_source: cached.structured_source,
        thread_source: cached.thread_source,
        parent_thread_id: cached.parent_thread_id,
        model_provider: cached.model_provider,
        // Also always overwritten by the caller from the scan root's kind.
        archived: false,
        user_messages: cached
            .user_messages
            .into_iter()
            .map(from_cached_message)
            .collect(),
        outcome: from_cached_file_outcome(cached.file_outcome),
        malformed_middle: cached.malformed_middle,
        import: cached.import.map(|import| ImportMeta {
            source_kind: import.source_kind,
        }),
        // SQLite enrichment always runs fresh on top of a cached-or-parsed
        // ParsedSession (see discover_with_filter_enriched); never cached.
        sqlite_title: None,
        sqlite_activity_time: None,
        sqlite_archived_hint: None,
    }
}

fn cache_message(message: &UserMessage) -> CachedUserMessage {
    CachedUserMessage {
        text: message.text.clone(),
        attachments: message.attachments.iter().map(cache_attachment).collect(),
    }
}

fn from_cached_message(cached: CachedUserMessage) -> UserMessage {
    UserMessage {
        text: cached.text,
        attachments: cached
            .attachments
            .into_iter()
            .map(from_cached_attachment)
            .collect(),
    }
}

fn cache_attachment(attachment: &Attachment) -> CachedAttachment {
    match attachment {
        Attachment::Image { media_type, .. } => CachedAttachment::Image {
            media_type: media_type.clone(),
        },
        Attachment::File { filename, .. } => CachedAttachment::File {
            filename: filename.clone(),
        },
        Attachment::Text { content } => CachedAttachment::Text {
            content: content.clone(),
        },
    }
}

fn from_cached_attachment(cached: CachedAttachment) -> Attachment {
    match cached {
        // Reconstructed via the constructors (not a bare struct literal) so
        // the placeholder `note` field stays the single source of truth
        // defined on `Attachment`, never duplicated into the cache.
        CachedAttachment::Image { media_type } => Attachment::image(media_type),
        CachedAttachment::File { filename } => Attachment::file(filename),
        CachedAttachment::Text { content } => Attachment::Text { content },
    }
}

fn cache_file_outcome(outcome: &FileOutcome) -> CachedFileOutcome {
    match outcome {
        FileOutcome::Complete => CachedFileOutcome::Complete,
        FileOutcome::IncompleteTail => CachedFileOutcome::IncompleteTail,
        FileOutcome::BoundExceeded => CachedFileOutcome::BoundExceeded,
    }
}

fn from_cached_file_outcome(cached: CachedFileOutcome) -> FileOutcome {
    match cached {
        CachedFileOutcome::Complete => FileOutcome::Complete,
        CachedFileOutcome::IncompleteTail => FileOutcome::IncompleteTail,
        CachedFileOutcome::BoundExceeded => FileOutcome::BoundExceeded,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_rollout(path: &Path, content: &[u8]) {
        let mut f = fs::File::create(path).unwrap();
        f.write_all(content).unwrap();
    }

    fn sample_parsed(rollout_path: PathBuf) -> ParsedSession {
        ParsedSession {
            rollout_path,
            effective_root: Some(PathBuf::from("/root")),
            id: "abc-123".into(),
            cwd: Some(PathBuf::from("/workspace")),
            timestamp: Some("2026-01-01T00:00:00Z".into()),
            cli_version: Some("0.1.0".into()),
            originator: Some("cli".into()),
            source: None,
            structured_source: Some(serde_json::json!({
                "subagent": { "thread_spawn": { "parent_thread_id": "parent-1" } }
            })),
            thread_source: Some("subagent".into()),
            parent_thread_id: Some("parent-1".into()),
            model_provider: Some("openai".into()),
            archived: true,
            user_messages: vec![UserMessage {
                text: "fix the bug".into(),
                attachments: vec![
                    Attachment::image(Some("image/png".into())),
                    Attachment::file(Some("notes.txt".into())),
                    Attachment::Text {
                        content: "code block".into(),
                    },
                ],
            }],
            outcome: FileOutcome::Complete,
            malformed_middle: 2,
            import: Some(ImportMeta {
                source_kind: Some("claude".into()),
            }),
            sqlite_title: Some("should not survive caching".into()),
            sqlite_activity_time: Some(std::time::UNIX_EPOCH),
            sqlite_archived_hint: Some(true),
        }
    }

    #[test]
    fn round_trips_a_session_through_record_and_lookup() {
        let dir = tempfile::tempdir().unwrap();
        let rollout = dir.path().join("rollout-1.jsonl");
        write_rollout(&rollout, b"{}\n");
        let metadata = fs::metadata(&rollout).unwrap();
        let mtime = metadata.modified().unwrap();

        let cache = DiscoveryCache::load(None);
        let original = sample_parsed(rollout.clone());
        cache.record(&rollout, metadata.len(), mtime, Some(&original));

        let hit = cache.lookup(&rollout).expect("cache hit").expect("session");
        assert_eq!(hit.id, original.id);
        assert_eq!(hit.cwd, original.cwd);
        assert_eq!(hit.structured_source, original.structured_source);
        assert_eq!(hit.parent_thread_id, original.parent_thread_id);
        assert_eq!(hit.user_messages, original.user_messages);
        assert_eq!(hit.import.unwrap().to_display(), "imported from claude");
        assert_eq!(hit.malformed_middle, 2);
        assert_eq!(hit.outcome, FileOutcome::Complete);
        // Enrichment hints are never cached -- always fresh per run.
        assert_eq!(hit.sqlite_title, None);
        assert_eq!(hit.sqlite_activity_time, None);
        assert_eq!(hit.sqlite_archived_hint, None);
        // Overwritten unconditionally by the caller after every parse.
        assert_eq!(hit.effective_root, None);
        assert!(!hit.archived);
    }

    #[test]
    fn records_and_looks_up_a_no_session_result() {
        let dir = tempfile::tempdir().unwrap();
        let rollout = dir.path().join("empty.jsonl");
        write_rollout(&rollout, b"not a session\n");
        let metadata = fs::metadata(&rollout).unwrap();
        let mtime = metadata.modified().unwrap();

        let cache = DiscoveryCache::load(None);
        cache.record(&rollout, metadata.len(), mtime, None);
        assert!(matches!(cache.lookup(&rollout), Some(None)));
    }

    #[test]
    fn misses_when_the_file_was_never_recorded() {
        let dir = tempfile::tempdir().unwrap();
        let rollout = dir.path().join("unknown.jsonl");
        write_rollout(&rollout, b"{}\n");
        let cache = DiscoveryCache::load(None);
        assert!(cache.lookup(&rollout).is_none());
    }

    #[test]
    fn misses_after_the_file_is_modified() {
        let dir = tempfile::tempdir().unwrap();
        let rollout = dir.path().join("rollout.jsonl");
        write_rollout(&rollout, b"{}\n");
        let metadata = fs::metadata(&rollout).unwrap();
        let mtime = metadata.modified().unwrap();
        let cache = DiscoveryCache::load(None);
        cache.record(
            &rollout,
            metadata.len(),
            mtime,
            Some(&sample_parsed(rollout.clone())),
        );
        assert!(cache.lookup(&rollout).is_some());

        // Simulate a still-growing active session: content and mtime change.
        std::thread::sleep(std::time::Duration::from_millis(10));
        write_rollout(&rollout, b"{}\n{}\n");
        assert!(
            cache.lookup(&rollout).is_none(),
            "a changed file must never serve a stale cached parse"
        );
    }

    #[test]
    fn persists_across_load_save_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cache").join(CACHE_FILE_NAME);
        let root = dir.path();

        let rollout_a = root.join("a.jsonl");
        let rollout_b = root.join("b.jsonl");
        write_rollout(&rollout_a, b"{}\n");
        write_rollout(&rollout_b, b"{}\n");
        let meta_a = fs::metadata(&rollout_a).unwrap();
        let meta_b = fs::metadata(&rollout_b).unwrap();
        let seen = [rollout_a.clone(), rollout_b.clone()];

        let cache = DiscoveryCache::load(Some(path.clone()));
        cache.record(
            &rollout_a,
            meta_a.len(),
            meta_a.modified().unwrap(),
            Some(&sample_parsed(rollout_a.clone())),
        );
        cache.record(&rollout_b, meta_b.len(), meta_b.modified().unwrap(), None);
        cache.save(root, &seen);

        let cache2 = DiscoveryCache::load(Some(path.clone()));
        assert!(
            cache2.lookup(&rollout_a).is_some(),
            "a must survive a reload"
        );
        assert!(
            matches!(cache2.lookup(&rollout_b), Some(None)),
            "b must survive a reload"
        );
        cache2.save(root, &seen); // no new records: must be a no-op, not truncate the file.

        let cache3 = DiscoveryCache::load(Some(path));
        assert!(cache3.lookup(&rollout_a).is_some());
        assert!(matches!(cache3.lookup(&rollout_b), Some(None)));
    }

    #[test]
    fn save_prunes_an_entry_for_a_file_deleted_since_it_was_cached() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(CACHE_FILE_NAME);
        let root = dir.path();

        let rollout_a = root.join("a.jsonl");
        let rollout_b = root.join("b.jsonl");
        write_rollout(&rollout_a, b"{}\n");
        write_rollout(&rollout_b, b"{}\n");
        let meta_a = fs::metadata(&rollout_a).unwrap();
        let meta_b = fs::metadata(&rollout_b).unwrap();

        // Run 1: both files exist, both recorded, both seen.
        let cache = DiscoveryCache::load(Some(path.clone()));
        cache.record(
            &rollout_a,
            meta_a.len(),
            meta_a.modified().unwrap(),
            Some(&sample_parsed(rollout_a.clone())),
        );
        cache.record(&rollout_b, meta_b.len(), meta_b.modified().unwrap(), None);
        cache.save(root, &[rollout_a.clone(), rollout_b.clone()]);

        // Between runs, `b` is deleted -- a real `discover_with_filter_
        // enriched` pass over `root` would no longer find it in
        // `list_rollout_files`, so it is never in `seen` again.
        fs::remove_file(&rollout_b).unwrap();

        // Run 2: only `a` is seen (the true current file list under `root`).
        let cache2 = DiscoveryCache::load(Some(path.clone()));
        cache2.save(root, std::slice::from_ref(&rollout_a));

        let cache3 = DiscoveryCache::load(Some(path));
        assert!(
            cache3.lookup(&rollout_a).is_some(),
            "a is still on disk and must survive pruning"
        );
        let deleted_path = std::path::Path::new(&rollout_b);
        assert!(
            fs::metadata(deleted_path).is_err(),
            "sanity: b really is deleted"
        );
    }

    #[test]
    fn save_never_prunes_entries_under_a_different_effective_root() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(CACHE_FILE_NAME);
        let root_x = dir.path().join("codex_home_x");
        let root_y = dir.path().join("codex_home_y");
        fs::create_dir_all(&root_x).unwrap();
        fs::create_dir_all(&root_y).unwrap();

        let rollout_x = root_x.join("x.jsonl");
        write_rollout(&rollout_x, b"{}\n");
        let meta_x = fs::metadata(&rollout_x).unwrap();

        // Run 1: a rollout cached under a nonstandard CODEX_HOME (`root_x`).
        let cache = DiscoveryCache::load(Some(path.clone()));
        cache.record(
            &rollout_x,
            meta_x.len(),
            meta_x.modified().unwrap(),
            Some(&sample_parsed(rollout_x.clone())),
        );
        cache.save(&root_x, std::slice::from_ref(&rollout_x));

        // Run 2: a *different* CODEX_HOME (`root_y`) is scanned; `root_x`'s
        // rollout is never seen (or even known about) this run.
        let cache2 = DiscoveryCache::load(Some(path));
        cache2.save(&root_y, &[]);

        let reloaded = DiscoveryCache::load(Some(dir.path().join(CACHE_FILE_NAME)));
        assert!(
            reloaded.lookup(&rollout_x).is_some(),
            "an entry outside this run's effective_root must never be pruned"
        );
    }

    #[test]
    fn a_corrupt_cache_file_loads_as_empty_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(CACHE_FILE_NAME);
        fs::write(&path, b"not json at all").unwrap();
        let cache = DiscoveryCache::load(Some(path));
        let rollout = dir.path().join("x.jsonl");
        write_rollout(&rollout, b"{}\n");
        assert!(cache.lookup(&rollout).is_none());
    }

    #[test]
    fn a_version_mismatched_cache_file_loads_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(CACHE_FILE_NAME);
        fs::write(&path, br#"{"version":999,"entries":{}}"#).unwrap();
        let cache = DiscoveryCache::load(Some(path));
        let rollout = dir.path().join("x.jsonl");
        write_rollout(&rollout, b"{}\n");
        assert!(cache.lookup(&rollout).is_none());
    }

    #[test]
    fn a_pathless_cache_still_memoizes_within_a_run_but_save_never_touches_disk() {
        let dir = tempfile::tempdir().unwrap();
        let rollout = dir.path().join("x.jsonl");
        write_rollout(&rollout, b"{}\n");
        let metadata = fs::metadata(&rollout).unwrap();
        // No resolvable path (e.g. neither XDG_CACHE_HOME nor HOME is set):
        // load(None) still memoizes for the current run in `fresh` -- a
        // rollout visited twice in one pass (active + archived roots)
        // still benefits -- but `save` has nothing to write to.
        let cache = DiscoveryCache::load(None);
        cache.record(
            &rollout,
            metadata.len(),
            metadata.modified().unwrap(),
            Some(&sample_parsed(rollout.clone())),
        );
        assert!(cache.lookup(&rollout).is_some());
        cache.save(dir.path(), &[rollout]); // must not panic or attempt any I/O.
    }

    #[test]
    fn cache_path_prefers_xdg_cache_home_over_home() {
        let xdg = PathBuf::from("/xdg/cache");
        let home = PathBuf::from("/home/user");
        assert_eq!(
            cache_path(Some(xdg.clone()), Some(home.clone())),
            Some(xdg.join("resume").join(CACHE_FILE_NAME))
        );
        assert_eq!(
            cache_path(None, Some(home.clone())),
            Some(home.join(".cache/resume").join(CACHE_FILE_NAME))
        );
        assert_eq!(cache_path(None, None), None);
    }
}
