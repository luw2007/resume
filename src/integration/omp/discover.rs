//! Read-only OMP transcript discovery.

use super::{
    format::{self, ParsedSession},
    roots::EffectiveRoots,
};
use crate::{
    preview::jsonl::{self, Bounds, FileOutcome, ReadResult},
    scope::Scope,
};
use std::{
    fs, io,
    path::{Path, PathBuf},
};

const DISCOVERY_SCAN_RECORDS: usize = 50_000;

/// Configuration for an OMP discovery pass.
#[derive(Clone, Debug)]
pub struct DiscoverConfig<'a> {
    /// Effective OMP roots (owned so callers can pass temporary values).
    pub roots: EffectiveRoots,
    /// Scope used to filter Sessions by header `cwd`.
    pub scope: &'a Scope,
    /// Bounds for the JSONL reader. Discovery uses a record cap.
    pub bounds: Bounds,
    /// Canonical home directory for the grouped-directory-name prefilter.
    home: Option<PathBuf>,
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
            home: None,
        }
        .with_home(std::env::var_os("HOME").map(PathBuf::from))
    }

    /// Set the home directory used by OMP's home-relative directory prefilter.
    /// Canonicalization keeps it comparable with the canonical Scope base when
    /// `$HOME` is a symlink, as on Linux hosts with data volumes mounted elsewhere.
    pub fn with_home(mut self, home: Option<PathBuf>) -> Self {
        self.home = home.map(|path| path.canonicalize().unwrap_or(path));
        self
    }
}

/// Outcome of discovering OMP sessions in the effective session root.
#[derive(Clone, Debug, Default)]
pub struct DiscoverOutcome {
    /// Parsed sessions, before dedupe and Session construction.
    pub parsed: Vec<ParsedSession>,
    /// Number of JSONL files skipped due to read/parse errors (aggregated).
    pub skipped_files: usize,
    /// Number of files with no valid `session` header.
    pub no_header_files: usize,
    /// Number of files skipped because the header `cwd` was outside Scope.
    pub out_of_scope: usize,
    /// Number of grouped Workspace directories pruned by the directory-name
    /// prefilter without reading any file inside them.
    pub pruned_dirs: usize,
}

/// Discover OMP sessions under the effective session root. Reads JSONL
/// read-only through the shared reader, parses the title sidecar + v3 header
/// (never assuming the header is the first record), and filters by header
/// `cwd` through Scope. Never invokes OMP or migrates files.
///
/// Discovery scans `.jsonl` files one level (or more) under the session root.
/// In the default grouped layout, encoded Workspace directory names are
/// always dash-prefixed and serve as a lossy Scope prefilter: a directory
/// whose name cannot correspond to any in-Scope Workspace is skipped without
/// reading its files (`Scope::may_contain_session_dir`). Custom session
/// roots (`custom_session_root`) are never pruned. Header `cwd` stays
/// authoritative for every file that is read.
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
                // Missing Workspace: surfaced for diagnosis (Unavailable).
            }
        }

        outcome.parsed.push(parsed);
    }

    Ok(outcome)
}

/// Enumerate `.jsonl` files reachable from the session root. Tolerates a
/// missing session root (returns empty).
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
    paths.sort();
    Ok(paths)
}

/// Recursively collect `.jsonl` file paths over the storage layout. Encoded
/// Workspace directory names always start with `-` (home-relative or
/// absolute lossy encoding); such a directory whose name cannot encode any
/// in-Scope Workspace is pruned without reading it (counted in
/// `outcome.pruned_dirs`). Any other directory (e.g. the literal `sessions`
/// level under the agent root) is descended unconditionally, and custom
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

/// Parse a single OMP JSONL session file read-only. Returns `Ok(None)` when
/// the file has no valid `session` header.
fn parse_session_file(
    path: &Path,
    effective_root: &Path,
    bounds: &Bounds,
) -> io::Result<Option<ParsedSession>> {
    let result = jsonl::read_file_confined(path, effective_root, bounds)?;
    let file_mtime = fs::metadata(path).and_then(|m| m.modified()).ok();
    Ok(format::extract_session(path, &result, file_mtime))
}

/// Whether a read result indicates a file that was being actively written.
#[allow(dead_code)]
pub(super) fn was_live_growing(result: &ReadResult) -> bool {
    matches!(result.outcome, FileOutcome::IncompleteTail)
}
