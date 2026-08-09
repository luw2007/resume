//! Discovery and Scope-matching performance benchmarks.
//!
//! ## Why this exists
//!
//! `resume --json --up all` against a real, populated `~/.codex/sessions`
//! (thousands of rollout files, hundreds of distinct workspaces) was observed
//! to take 20-40s wall-clock, dominated by two effects this suite isolates
//! and tracks separately so a regression in either shows up on its own axis:
//!
//! 1. **`Scope::contains_workspace` git subprocess overhead** (`git_scope`
//!    group): every Session's Workspace was checked against Scope by
//!    spawning `git rev-parse` per call, even in `--up`/`--down`/non-Git
//!    Scope modes where the result is never consulted, and even when many
//!    Sessions share the same Workspace. Fixed by only spawning in
//!    `ScopeMode::Git` and memoizing per canonical path
//!    (`docs/product-design.md` section 4: "Query each normalized Workspace
//!    at most once").
//! 2. **Full-file JSONL parsing for title derivation** (`codex_discovery`
//!    group): every rollout file is read and every line parsed as JSON to
//!    find `session_meta` and derive a title, even though large rollouts
//!    (tens of MB) only need `session_meta` (near the file start) plus a
//!    bounded early read for the first user message
//!    (`docs/product-design.md` section 3: "at most a 1 MiB bounded early
//!    read"). This group demonstrates but does not yet fix that gap.
//!
//! ## Fixtures
//!
//! No real Session data is used or committed. `fixtures::codex_tree`
//! generates a synthetic `CODEX_HOME`-shaped directory tree with a
//! controllable file count, per-file size, and Workspace fan-out (many
//! rollouts sharing few distinct `cwd` values, matching the real-world shape
//! that made per-Session `git rev-parse` so costly). Regenerated fresh for
//! every `criterion` run in a `TempDir`; nothing here reads `$HOME` or any
//! real agent store.
//!
//! ## Running
//!
//! ```sh
//! cargo bench --bench discovery
//! cargo bench --bench discovery -- git_scope   # one group only
//! ```

use std::{hint::black_box, path::PathBuf};

use criterion::{Criterion, criterion_group, criterion_main};
use resume::{
    integration::codex,
    preview::jsonl::Bounds,
    scope::{DefaultScope, Scope},
};
use tempfile::TempDir;

mod fixtures;

/// Benchmark `Scope::contains_workspace` in `ScopeMode::Git`, the only mode
/// where the fix's memoization applies. Uses a real temporary Git repository
/// (not a synthetic double) so the benchmark exercises the actual
/// `git rev-parse --git-common-dir` subprocess path, not a mock.
fn bench_git_scope(c: &mut Criterion) {
    let repo = fixtures::init_git_repo();
    let scope = Scope::new(
        repo.path().to_path_buf(),
        None,
        DefaultScope::Git {
            common_dir: repo.path().join(".git"),
            worktrees: vec![repo.path().to_path_buf()],
        },
    );
    // A realistic fan-in: many Sessions (calls), few distinct Workspaces.
    let workspaces: Vec<PathBuf> = (0..20)
        .map(|i| repo.path().join(format!("sub{i}")))
        .collect();
    for ws in &workspaces {
        std::fs::create_dir_all(ws).unwrap();
    }

    let mut group = c.benchmark_group("git_scope");
    group.bench_function(
        "contains_workspace_repeated_1000_calls_20_unique_paths",
        |b| {
            b.iter(|| {
                for i in 0..1000 {
                    let ws = &workspaces[i % workspaces.len()];
                    black_box(scope.contains_workspace(ws));
                }
            });
        },
    );
    group.finish();
}

/// Benchmark full Codex discovery against a synthetic rollout tree at a
/// scale representative of a long-lived real `~/.codex/sessions` (thousands
/// of files, tens-of-MB outliers, workspace fan-in).
fn bench_codex_discovery(c: &mut Criterion) {
    let mut group = c.benchmark_group("codex_discovery");
    group.sample_size(10); // large fixture; keep wall-clock bounded per run

    for &(files, avg_lines, big_files) in &[(200usize, 50usize, 0usize), (200, 50, 2)] {
        let root: TempDir = fixtures::codex_tree(files, avg_lines, big_files, 40);
        let bench_name = if big_files == 0 {
            format!("uniform_{files}files_{avg_lines}lines")
        } else {
            format!("uniform_{files}files_{avg_lines}lines_plus_{big_files}x40MB")
        };
        group.bench_function(bench_name, |b| {
            b.iter(|| {
                let out = codex::discover(root.path(), &Bounds::default());
                black_box(out.len());
            });
        });
    }
    group.finish();
}

criterion_group!(benches, bench_git_scope, bench_codex_discovery);
criterion_main!(benches);
