use std::{
    ffi::OsStr,
    io,
    path::{Path, PathBuf},
    process::Command,
};

use crate::{
    cli::Distance,
    session::{RiskStatus, WorkspaceEvidence},
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Direction {
    Up(Distance),
    Down(Distance),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DefaultScope {
    Git {
        common_dir: PathBuf,
        worktrees: Vec<PathBuf>,
    },
    Exact {
        git_warning: Option<String>,
    },
}

/// Not `Clone`/`PartialEq`/`Eq`: `git_common_dir_cache` holds process-local
/// memoized subprocess results (see `contains_workspace`) and is shared via
/// `Arc<Scope>` across per-agent discovery threads rather than duplicated by
/// value or compared for equality anywhere in the codebase.
#[derive(Debug)]
pub struct Scope {
    base: PathBuf,
    mode: ScopeMode,
    git_warning: Option<String>,
    git_common_dir_cache: std::sync::Mutex<std::collections::HashMap<PathBuf, Option<PathBuf>>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum ScopeMode {
    Up(Distance),
    Down(Distance),
    Git {
        common_dir: PathBuf,
        worktrees: Vec<PathBuf>,
    },
    Exact,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceCandidate<'a> {
    pub real_path: &'a Path,
    /// Git common directory for the candidate, when positively identified.
    pub git_common_dir: Option<&'a Path>,
    pub exists: bool,
}

impl Scope {
    pub fn new(base: PathBuf, direction: Option<Direction>, default: DefaultScope) -> Self {
        let git_warning = match &default {
            DefaultScope::Exact { git_warning } => git_warning.clone(),
            DefaultScope::Git { .. } => None,
        };
        let mode = match direction {
            Some(Direction::Up(distance)) => ScopeMode::Up(distance),
            Some(Direction::Down(distance)) => ScopeMode::Down(distance),
            None => match default {
                DefaultScope::Git {
                    common_dir,
                    worktrees,
                } => ScopeMode::Git {
                    common_dir,
                    worktrees,
                },
                DefaultScope::Exact { .. } => ScopeMode::Exact,
            },
        };
        Self {
            base,
            mode,
            git_warning,
            git_common_dir_cache: std::sync::Mutex::new(std::collections::HashMap::new()),
        }
    }

    pub fn contains(&self, candidate: WorkspaceCandidate<'_>) -> bool {
        match &self.mode {
            ScopeMode::Exact => candidate.real_path == self.base,
            ScopeMode::Up(distance) => self
                .base
                .ancestors()
                .position(|path| path == candidate.real_path)
                .is_some_and(|edges| within(distance, edges)),
            ScopeMode::Down(distance) => candidate
                .real_path
                .strip_prefix(&self.base)
                .ok()
                .is_some_and(|suffix| within(distance, suffix.components().count())),
            ScopeMode::Git {
                common_dir,
                worktrees,
            } => {
                if candidate
                    .git_common_dir
                    .is_some_and(|candidate_common| candidate_common != common_dir)
                {
                    return false;
                }
                worktrees
                    .iter()
                    .any(|worktree| candidate.real_path.starts_with(worktree))
            }
        }
    }

    /// Match a recorded Workspace using its canonical path when it still
    /// exists, or its last-known absolute path when it has disappeared.
    ///
    /// Per `docs/product-design.md` section 4 ("Git metadata performance"):
    /// "Query each normalized Workspace at most once" and cache only for the
    /// current process. `git_common_dir` is a `git rev-parse` subprocess
    /// spawn, so three optimizations apply:
    ///
    /// 1. It is computed only in `ScopeMode::Git`, the only mode that reads
    ///    it (`contains` ignores `git_common_dir` in every other mode) --
    ///    skipping the spawn entirely for `--up`/`--down`/non-Git Scopes.
    /// 2. In `ScopeMode::Git`, results are cached per canonical path for the
    ///    lifetime of this `Scope`, so a workspace shared by many Sessions
    ///    (a common case: hundreds of transcripts under the same repo) pays
    ///    the subprocess cost once, not once per Session. `Scope` is shared
    ///    via `Arc` across per-agent discovery threads, so the cache is a
    ///    `Mutex`, not a `RefCell`.
    /// 3. In `ScopeMode::Git`, `git_common_dir` is only ever consulted to
    ///    *narrow* a match (exclude a distinct nested repository) --
    ///    `contains()`'s Git branch never returns `true` unless `real_path`
    ///    also starts with one of the current repository's worktrees. So a
    ///    candidate whose Workspace does not even have that (subprocess-free)
    ///    prefix in common with the repository can never match regardless of
    ///    `git_common_dir`, and the spawn behind it is skipped entirely for
    ///    that candidate. Measured against a real Session history spanning
    ///    many unrelated projects (the common case: an agent's Session
    ///    history is not scoped to one repository), only ~1% of distinct
    ///    recorded Workspaces shared a prefix with the current repository's
    ///    worktrees, so this ordering alone removed the subprocess spawn for
    ///    roughly 99% of distinct Workspaces without changing which Sessions
    ///    are in Scope.
    pub fn contains_workspace(&self, workspace: &Path) -> bool {
        let canonical = canonical_workspace(workspace);
        let last_known_real = canonical
            .clone()
            .or_else(|| resolve_missing_workspace_path(workspace));
        let real_path = last_known_real.as_deref().unwrap_or(workspace);

        if let ScopeMode::Git { worktrees, .. } = &self.mode
            && !worktrees
                .iter()
                .any(|worktree| real_path.starts_with(worktree))
        {
            return false;
        }

        let git_common_dir = if matches!(self.mode, ScopeMode::Git { .. }) {
            canonical
                .as_deref()
                .and_then(|path| self.cached_git_common_dir(path))
        } else {
            None
        };
        self.contains(WorkspaceCandidate {
            real_path,
            git_common_dir: git_common_dir.as_deref(),
            exists: canonical.is_some(),
        })
    }

    /// Look up (and memoize) `git_common_dir` for a canonical path. Only
    /// called from `ScopeMode::Git`, where `contains` actually consults the
    /// result. Poison-tolerant: a panicked holder cannot silently disable
    /// the entire Scope, so a poisoned lock falls back to an uncached call.
    fn cached_git_common_dir(&self, canonical_path: &Path) -> Option<PathBuf> {
        if let Ok(cache) = self.git_common_dir_cache.lock()
            && let Some(hit) = cache.get(canonical_path)
        {
            return hit.clone();
        }
        let resolved = workspace_git_common_dir(canonical_path);
        if let Ok(mut cache) = self.git_common_dir_cache.lock() {
            cache.insert(canonical_path.to_path_buf(), resolved.clone());
        }
        resolved
    }

    pub fn base(&self) -> &Path {
        &self.base
    }

    pub fn git_warning(&self) -> Option<&str> {
        self.git_warning.as_deref()
    }
}

fn within(distance: &Distance, edges: usize) -> bool {
    match distance {
        Distance::Finite(max) => edges <= *max,
        Distance::All => true,
    }
}

pub fn canonical_base(path: &Path) -> io::Result<PathBuf> {
    path.canonicalize()
}

pub fn canonical_workspace(path: &Path) -> Option<PathBuf> {
    path.canonicalize().ok()
}

/// Resolve symlinks in the nearest existing ancestor while retaining the
/// missing suffix. This preserves the last-known real path on platforms where
/// lexical absolute paths (for example `/var`) alias another real path.
fn resolve_missing_workspace_path(path: &Path) -> Option<PathBuf> {
    let mut ancestor = path;
    let mut suffix = Vec::new();
    loop {
        if let Ok(real) = ancestor.canonicalize() {
            return Some(
                suffix
                    .iter()
                    .rev()
                    .fold(real, |resolved, component| resolved.join(component)),
            );
        }
        suffix.push(ancestor.file_name()?.to_os_string());
        ancestor = ancestor.parent()?;
    }
}

fn workspace_git_common_dir(path: &Path) -> Option<PathBuf> {
    let output = Command::new("git")
        .args([
            OsStr::new("-C"),
            path.as_os_str(),
            OsStr::new("rev-parse"),
            OsStr::new("--path-format=absolute"),
            OsStr::new("--git-common-dir"),
        ])
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| bytes_to_path(trim_newline(&output.stdout)))
}

pub fn broad_workspace_risk(evidence: &WorkspaceEvidence, home: Option<&Path>) -> RiskStatus {
    let Some(workspace) = evidence.workspace() else {
        return RiskStatus::Normal;
    };
    if workspace == Path::new("/") || home.is_some_and(|home| workspace == home) {
        RiskStatus::BroadWorkspace
    } else {
        RiskStatus::Normal
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitScopeEvidence {
    pub common_dir: PathBuf,
    pub worktrees: Vec<PathBuf>,
}

/// Discover Git Scope evidence for `base`.
///
/// When `all_worktrees` is `false` (the default -- see
/// `docs/product-design.md` section 4, "Default Scope"), only the current
/// worktree is queried and returned as the sole entry in `worktrees`: a
/// single `git rev-parse --git-common-dir --show-toplevel` call resolves
/// both the common directory and the current worktree's root together, so
/// this path costs exactly one subprocess spawn regardless of how many
/// linked worktrees the repository has.
///
/// When `all_worktrees` is `true` (`--all-worktrees`), a second `git
/// worktree list --porcelain` call additionally enumerates every linked
/// worktree, matching the pre-existing default-Scope behavior.
pub fn discover_git_scope(base: &Path, all_worktrees: bool) -> io::Result<GitScopeEvidence> {
    let common_output = Command::new("git")
        .args([
            OsStr::new("-C"),
            base.as_os_str(),
            OsStr::new("rev-parse"),
            OsStr::new("--path-format=absolute"),
            OsStr::new("--git-common-dir"),
            OsStr::new("--show-toplevel"),
        ])
        .output()?;
    if !common_output.status.success() {
        return Err(io::Error::other("git rev-parse failed"));
    }
    let mut lines = common_output.stdout.split(|byte| *byte == b'\n');
    let common_dir = bytes_to_path(trim_newline(lines.next().unwrap_or_default()));
    let toplevel = bytes_to_path(trim_newline(lines.next().unwrap_or_default()));

    if !all_worktrees {
        return Ok(GitScopeEvidence {
            common_dir,
            worktrees: vec![toplevel],
        });
    }

    let output = Command::new("git")
        .args([
            OsStr::new("-C"),
            base.as_os_str(),
            OsStr::new("worktree"),
            OsStr::new("list"),
            OsStr::new("--porcelain"),
            OsStr::new("-z"),
        ])
        .output()?;
    if !output.status.success() {
        return Err(io::Error::other("git worktree list failed"));
    }
    let worktrees = parse_worktree_porcelain_z(&output.stdout)?;
    Ok(GitScopeEvidence {
        common_dir,
        worktrees,
    })
}

pub fn parse_worktree_porcelain_z(output: &[u8]) -> io::Result<Vec<PathBuf>> {
    output
        .split(|byte| *byte == 0)
        .filter_map(|field| field.strip_prefix(b"worktree "))
        .map(|path| {
            let path = bytes_to_path(path);
            path.canonicalize()
        })
        .collect()
}

fn trim_newline(mut bytes: &[u8]) -> &[u8] {
    while matches!(bytes.last(), Some(b'\n' | b'\r')) {
        bytes = &bytes[..bytes.len() - 1];
    }
    bytes
}

#[cfg(unix)]
fn bytes_to_path(bytes: &[u8]) -> PathBuf {
    use std::os::unix::ffi::OsStrExt;
    PathBuf::from(OsStr::from_bytes(bytes))
}

#[cfg(not(unix))]
fn bytes_to_path(bytes: &[u8]) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(bytes).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(path: &str) -> WorkspaceCandidate<'_> {
        WorkspaceCandidate {
            real_path: Path::new(path),
            git_common_dir: None,
            exists: true,
        }
    }

    #[test]
    fn table_driven_directory_distance_matrix() {
        struct Case {
            name: &'static str,
            direction: Direction,
            path: &'static str,
            expected: bool,
        }
        let cases = [
            Case {
                name: "up zero self",
                direction: Direction::Up(Distance::Finite(0)),
                path: "/a/b",
                expected: true,
            },
            Case {
                name: "up zero parent",
                direction: Direction::Up(Distance::Finite(0)),
                path: "/a",
                expected: false,
            },
            Case {
                name: "up one parent",
                direction: Direction::Up(Distance::Finite(1)),
                path: "/a",
                expected: true,
            },
            Case {
                name: "up excludes child",
                direction: Direction::Up(Distance::All),
                path: "/a/b/c",
                expected: false,
            },
            Case {
                name: "up all root",
                direction: Direction::Up(Distance::All),
                path: "/",
                expected: true,
            },
            Case {
                name: "down zero self",
                direction: Direction::Down(Distance::Finite(0)),
                path: "/a/b",
                expected: true,
            },
            Case {
                name: "down zero child",
                direction: Direction::Down(Distance::Finite(0)),
                path: "/a/b/c",
                expected: false,
            },
            Case {
                name: "down two",
                direction: Direction::Down(Distance::Finite(2)),
                path: "/a/b/c/d",
                expected: true,
            },
            Case {
                name: "down excludes sibling",
                direction: Direction::Down(Distance::All),
                path: "/a/x",
                expected: false,
            },
            Case {
                name: "root down all",
                direction: Direction::Down(Distance::All),
                path: "/any/depth",
                expected: true,
            },
        ];
        for case in cases {
            let base = if case.name == "root down all" {
                "/"
            } else {
                "/a/b"
            };
            let scope = Scope::new(
                PathBuf::from(base),
                Some(case.direction),
                DefaultScope::Exact { git_warning: None },
            );
            assert_eq!(
                scope.contains(candidate(case.path)),
                case.expected,
                "{}",
                case.name
            );
        }
    }

    #[test]
    fn git_scope_includes_main_linked_and_deep_but_excludes_nested_repo() {
        let scope = Scope::new(
            "/repo".into(),
            None,
            DefaultScope::Git {
                common_dir: "/repo/.git".into(),
                worktrees: vec!["/repo".into(), "/linked".into()],
            },
        );
        for path in ["/repo", "/repo/deep", "/linked", "/linked/deep"] {
            assert!(scope.contains(candidate(path)), "{path}");
        }
        assert!(!scope.contains(candidate("/sibling")));
        assert!(!scope.contains(WorkspaceCandidate {
            real_path: Path::new("/repo/vendor/nested"),
            git_common_dir: Some(Path::new("/repo/vendor/nested/.git")),
            exists: true,
        }));
        assert!(scope.contains(WorkspaceCandidate {
            real_path: Path::new("/repo/deep"),
            git_common_dir: Some(Path::new("/repo/.git")),
            exists: true,
        }));
    }

    #[test]
    fn non_git_fallback_is_exact_and_missing_workspace_is_matched_by_last_known_path() {
        let scope = Scope::new(
            "/real/base".into(),
            None,
            DefaultScope::Exact {
                git_warning: Some("failed".into()),
            },
        );
        assert!(scope.contains(candidate("/real/base")));
        assert!(!scope.contains(candidate("/real/base/child")));
        assert!(scope.contains(WorkspaceCandidate {
            real_path: Path::new("/real/base"),
            git_common_dir: None,
            exists: false
        }));
    }

    #[test]
    fn live_git_scope_rejects_a_distinct_nested_repository() {
        let root = tempfile::tempdir().unwrap();
        let repo = root.path().join("repo");
        let nested = repo.join("vendor/nested");
        std::fs::create_dir_all(&nested).unwrap();
        assert!(
            Command::new("git")
                .args(["init", "-q"])
                .current_dir(&repo)
                .status()
                .unwrap()
                .success()
        );
        assert!(
            Command::new("git")
                .args(["init", "-q"])
                .current_dir(&nested)
                .status()
                .unwrap()
                .success()
        );

        let repo = repo.canonicalize().unwrap();
        let nested = nested.canonicalize().unwrap();
        let scope = Scope::new(
            repo.clone(),
            None,
            DefaultScope::Git {
                common_dir: repo.join(".git"),
                worktrees: vec![repo.clone()],
            },
        );

        assert!(scope.contains_workspace(&repo.join("vendor")));
        assert!(!scope.contains_workspace(&nested));
    }

    /// `docs/product-design.md` section 4 ("Git metadata performance"):
    /// "Query each normalized Workspace at most once." `contains_workspace`
    /// spawns `git rev-parse` per call in `ScopeMode::Git`; without
    /// memoization, looking up the same canonical path many times costs one
    /// subprocess spawn each time (multiple ms each). This asserts the
    /// *second* batch of identical-path calls is not asymptotically as
    /// expensive as the first -- a regression to "spawn every time" would
    /// make both batches equally slow, while a correctly memoized cache
    /// makes the second batch orders of magnitude faster. Uses a generous 5x
    /// threshold (not raw ms) to stay robust across slow/loaded CI runners
    /// while still catching a reintroduced per-call spawn.
    #[test]
    fn contains_workspace_memoizes_git_common_dir_per_canonical_path() {
        let root = tempfile::tempdir().unwrap();
        assert!(
            Command::new("git")
                .args(["init", "-q"])
                .current_dir(root.path())
                .status()
                .unwrap()
                .success()
        );
        let repo = root.path().canonicalize().unwrap();
        let scope = Scope::new(
            repo.clone(),
            None,
            DefaultScope::Git {
                common_dir: repo.join(".git"),
                worktrees: vec![repo.clone()],
            },
        );

        let first_batch = std::time::Instant::now();
        for _ in 0..20 {
            scope.contains_workspace(&repo);
        }
        let first_elapsed = first_batch.elapsed();

        let second_batch = std::time::Instant::now();
        for _ in 0..20 {
            scope.contains_workspace(&repo);
        }
        let second_elapsed = second_batch.elapsed();

        assert!(
            second_elapsed * 5 < first_elapsed.max(std::time::Duration::from_micros(1)),
            "expected cached lookups to be much faster: first={first_elapsed:?} second={second_elapsed:?}"
        );
    }

    #[test]
    fn missing_workspace_uses_its_last_known_path_for_scope_matching() {
        let scope = Scope::new(
            "/real/base".into(),
            Some(Direction::Down(Distance::All)),
            DefaultScope::Exact { git_warning: None },
        );

        assert!(scope.contains_workspace(Path::new("/real/base/deleted")));
        assert!(!scope.contains_workspace(Path::new("/other/deleted")));
    }

    #[test]
    fn canonicalization_resolves_symlinks_and_rejects_missing() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        std::fs::create_dir(&real).unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&real, dir.path().join("link")).unwrap();
            assert_eq!(
                canonical_base(&dir.path().join("link")).unwrap(),
                real.canonicalize().unwrap()
            );
        }
        assert!(canonical_workspace(&dir.path().join("missing")).is_none());
    }

    #[test]
    fn home_and_root_are_broad_workspace_risks() {
        for path in ["/", "/home/me"] {
            let evidence = WorkspaceEvidence::Recorded {
                workspace: path.into(),
                historical_git_identity: None,
            };
            assert_eq!(
                broad_workspace_risk(&evidence, Some(Path::new("/home/me"))),
                RiskStatus::BroadWorkspace
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn worktree_parser_preserves_non_utf8_path() {
        use std::os::unix::ffi::OsStrExt;
        let dir = tempfile::tempdir().unwrap();
        let mut raw = b"worktree ".to_vec();
        raw.extend_from_slice(dir.path().as_os_str().as_bytes());
        raw.push(0);
        let parsed = parse_worktree_porcelain_z(&raw).unwrap();
        assert_eq!(parsed, [dir.path().canonicalize().unwrap()]);
    }

    /// `docs/product-design.md` section 4 ("Default Scope"): by default,
    /// `discover_git_scope` must return only the current worktree, not every
    /// linked worktree of the repository -- a single `git rev-parse` call
    /// (not the additional `git worktree list` spawn) is enough to prove
    /// this without depending on `--all-worktrees`.
    #[test]
    fn discover_git_scope_default_returns_only_the_current_worktree() {
        let root = tempfile::tempdir().unwrap();
        let repo = root.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        assert!(
            Command::new("git")
                .args(["init", "-q"])
                .current_dir(&repo)
                .status()
                .unwrap()
                .success()
        );
        assert!(
            Command::new("git")
                .args(["commit", "--allow-empty", "-q", "-m", "init"])
                .current_dir(&repo)
                .status()
                .unwrap()
                .success()
        );
        let linked = root.path().join("linked");
        assert!(
            Command::new("git")
                .args([
                    "worktree",
                    "add",
                    "-q",
                    linked.to_str().unwrap(),
                    "-b",
                    "feature",
                ])
                .current_dir(&repo)
                .status()
                .unwrap()
                .success()
        );

        let repo = repo.canonicalize().unwrap();
        let linked = linked.canonicalize().unwrap();

        let narrow = discover_git_scope(&repo, false).unwrap();
        assert_eq!(narrow.worktrees, vec![repo.clone()]);

        let narrow_from_linked = discover_git_scope(&linked, false).unwrap();
        assert_eq!(narrow_from_linked.worktrees, vec![linked.clone()]);
        assert_eq!(narrow_from_linked.common_dir, narrow.common_dir);

        let wide = discover_git_scope(&repo, true).unwrap();
        assert_eq!(wide.common_dir, narrow.common_dir);
        assert!(wide.worktrees.contains(&repo));
        assert!(wide.worktrees.contains(&linked));
        assert_eq!(wide.worktrees.len(), 2);
    }
}
