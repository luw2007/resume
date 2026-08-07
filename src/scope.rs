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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Scope {
    base: PathBuf,
    mode: ScopeMode,
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
        Self { base, mode }
    }

    pub fn contains(&self, candidate: WorkspaceCandidate<'_>) -> bool {
        if !candidate.exists {
            return false;
        }
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

    pub fn base(&self) -> &Path {
        &self.base
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

pub fn discover_git_scope(base: &Path) -> io::Result<GitScopeEvidence> {
    let common_output = Command::new("git")
        .args([
            OsStr::new("-C"),
            base.as_os_str(),
            OsStr::new("rev-parse"),
            OsStr::new("--path-format=absolute"),
            OsStr::new("--git-common-dir"),
        ])
        .output()?;
    if !common_output.status.success() {
        return Err(io::Error::other("git rev-parse failed"));
    }
    let common_dir = bytes_to_path(trim_newline(&common_output.stdout));
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
    fn non_git_fallback_is_exact_and_missing_workspace_is_excluded() {
        let scope = Scope::new(
            "/real/base".into(),
            None,
            DefaultScope::Exact {
                git_warning: Some("failed".into()),
            },
        );
        assert!(scope.contains(candidate("/real/base")));
        assert!(!scope.contains(candidate("/real/base/child")));
        assert!(!scope.contains(WorkspaceCandidate {
            real_path: Path::new("/real/base"),
            git_common_dir: None,
            exists: false
        }));
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
}
