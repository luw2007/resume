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
//!    read"). Fixed for Codex; `pi_discovery`/`omp_discovery`/
//!    `claude_discovery` below track the same file-size sensitivity for the
//!    other three integrations, which cannot adopt the identical fix (see
//!    each group's doc comment for why).
//!
//! ## Fixtures
//!
//! No real Session data is used or committed. Every `fixtures::*_tree`
//! generator builds a synthetic, integration-shaped directory tree with a
//! controllable file count, per-file size, and Workspace fan-out (many
//! sessions sharing few distinct `cwd` values, matching the real-world shape
//! that made per-Session `git rev-parse` so costly for the `git_scope`
//! group). Regenerated fresh for every `criterion` run in a `TempDir`;
//! nothing here reads `$HOME` or any real agent store.
//!
//! ## Running
//!
//! ```sh
//! cargo bench --bench discovery
//! cargo bench --bench discovery -- git_scope   # one group only
//! cargo bench --bench discovery -- codex_discovery
//! cargo bench --bench discovery -- pi_discovery
//! cargo bench --bench discovery -- omp_discovery
//! cargo bench --bench discovery -- claude_discovery
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

        // Correctness sanity check, run once per fixture (not timed): the
        // fixture must actually be discoverable, or the benchmark would
        // silently measure a no-op instead of real discovery work.
        let sanity = codex::discover(root.path(), &Bounds::default());
        assert_eq!(
            sanity.len(),
            files + big_files,
            "codex_discovery fixture sanity check"
        );

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

        // Same fixture with a Workspace gate that rejects every rollout:
        // measures the out-of-scope fast path (small first-record read, no
        // title-derivation read), the common shape when discovery runs from
        // one repository against a many-project store.
        let gate = |_: &std::path::Path| false;
        let gated_sanity =
            codex::discover_with_filter(root.path(), &Bounds::default(), Some(&gate), |_| true);
        assert_eq!(
            gated_sanity
                .iter()
                .filter(|o| matches!(o, codex::DiscoveredSession::Session(_)))
                .count(),
            0,
            "gated fixture sanity check: everything out of scope"
        );
        let gated_name = if big_files == 0 {
            format!("gated_all_out_of_scope_{files}files_{avg_lines}lines")
        } else {
            format!("gated_all_out_of_scope_{files}files_{avg_lines}lines_plus_{big_files}x40MB")
        };
        group.bench_function(gated_name, |b| {
            b.iter(|| {
                let out = codex::discover_with_filter(
                    root.path(),
                    &Bounds::default(),
                    Some(&gate),
                    |_| true,
                );
                black_box(out.len());
            });
        });
    }
    group.finish();
}

/// Benchmark full Pi discovery against a synthetic grouped-session tree at a
/// scale comparable to `codex_discovery` (same file-count/line-count/
/// large-file parameters, so results are directly comparable across groups).
///
/// Pi cannot adopt Codex's "stop at the first user message" bounded-early-
/// read fix without changing correctness: it needs "latest wins" semantics
/// for `session_info.name` (the display title) and for the activity
/// timestamp (`latest_message_time`, computed by scanning every user message
/// in the file -- see `src/integration/pi/format.rs::extract_session`). This
/// group exists to make that file-size sensitivity visible and trackable
/// over time, not to demonstrate an already-applied fix.
fn bench_pi_discovery(c: &mut Criterion) {
    use resume::{
        integration::pi::{DiscoverConfig, EffectiveRoots},
        scope::Direction,
    };

    let mut group = c.benchmark_group("pi_discovery");
    group.sample_size(10);

    for &(files, avg_lines, big_files) in &[(200usize, 50usize, 0usize), (200, 50, 2)] {
        let tmp = tempfile::tempdir().expect("tempdir");
        let session_root = tmp.path().join("sessions");
        fixtures::grouped_session_tree(&session_root, files, avg_lines, big_files, 40);
        let roots = EffectiveRoots {
            agent_root: tmp.path().to_path_buf(),
            session_root: session_root.clone(),
            custom_session_root: false,
        };
        // `--down all`-equivalent: every discovered Workspace (all nested
        // under `tmp`) is in Scope. `tmp.path()` must be canonicalized before
        // constructing `Scope`, matching what `app::build_scope` does for the
        // real CLI (`scope::canonical_base`) -- on macOS `TMPDIR` resolves
        // through a `/var` -> `/private/var` symlink, so an uncanonicalized
        // base silently fails every `strip_prefix` check in `ScopeMode::Down`
        // and every Session is (silently) excluded as out-of-scope.
        let scope = Scope::new(
            tmp.path().canonicalize().unwrap(),
            Some(Direction::Down(resume::cli::Distance::All)),
            DefaultScope::Exact { git_warning: None },
        );

        let sanity =
            resume::integration::pi::discover(&DiscoverConfig::new(roots.clone(), &scope)).unwrap();
        assert_eq!(
            sanity.parsed.len(),
            files + big_files,
            "pi_discovery fixture sanity check"
        );

        let bench_name = if big_files == 0 {
            format!("uniform_{files}files_{avg_lines}lines")
        } else {
            format!("uniform_{files}files_{avg_lines}lines_plus_{big_files}x40MB")
        };
        group.bench_function(bench_name, |b| {
            b.iter(|| {
                let config = DiscoverConfig::new(roots.clone(), &scope);
                let outcome = resume::integration::pi::discover(&config).unwrap();
                black_box(outcome.parsed.len());
            });
        });
    }
    group.finish();
}

/// Benchmark full OMP discovery against a synthetic grouped-session tree.
/// Same rationale and scale as `bench_pi_discovery`: OMP's `title_change`
/// resolution likewise requires "latest wins" semantics (see
/// `src/integration/omp/format.rs::extract_session`), so it also cannot
/// adopt Codex's bounded-early-read fix without further correctness
/// research; this group tracks its file-size sensitivity.
fn bench_omp_discovery(c: &mut Criterion) {
    use resume::{
        integration::omp::{DiscoverConfig, EffectiveRoots, ProfileSelection},
        scope::Direction,
    };

    let mut group = c.benchmark_group("omp_discovery");
    group.sample_size(10);

    for &(files, avg_lines, big_files) in &[(200usize, 50usize, 0usize), (200, 50, 2)] {
        let tmp = tempfile::tempdir().expect("tempdir");
        let session_root = tmp.path().join("sessions");
        fixtures::grouped_session_tree(&session_root, files, avg_lines, big_files, 40);
        let roots = EffectiveRoots {
            config_root: tmp.path().to_path_buf(),
            config_root_overridden: false,
            agent_root: tmp.path().to_path_buf(),
            session_root: session_root.clone(),
            custom_session_root: false,
            profile: ProfileSelection::Default,
        };
        // See `bench_pi_discovery`'s comment: `--down all`-equivalent, base
        // canonicalized before constructing `Scope`.
        let scope = Scope::new(
            tmp.path().canonicalize().unwrap(),
            Some(Direction::Down(resume::cli::Distance::All)),
            DefaultScope::Exact { git_warning: None },
        );

        let sanity =
            resume::integration::omp::discover(&DiscoverConfig::new(roots.clone(), &scope))
                .unwrap();
        assert_eq!(
            sanity.parsed.len(),
            files + big_files,
            "omp_discovery fixture sanity check"
        );

        let bench_name = if big_files == 0 {
            format!("uniform_{files}files_{avg_lines}lines")
        } else {
            format!("uniform_{files}files_{avg_lines}lines_plus_{big_files}x40MB")
        };
        group.bench_function(bench_name, |b| {
            b.iter(|| {
                let config = DiscoverConfig::new(roots.clone(), &scope);
                let outcome = resume::integration::omp::discover(&config).unwrap();
                black_box(outcome.parsed.len());
            });
        });
    }
    group.finish();
}

/// Benchmark full Claude discovery against a synthetic project tree. Unlike
/// Pi/OMP, Claude's fields are "first non-empty wins" (see
/// `src/integration/claude/format.rs::interpret_record`), which is
/// structurally early-exit-friendly -- but `agent_name`/`ai_title` are not
/// guaranteed to appear near the start of a real transcript, so this group
/// currently measures the existing full-file-parse behavior rather than an
/// applied fix, same as Pi/OMP. It is the most file-size-sensitive of the
/// three at benchmark scale.
fn bench_claude_discovery(c: &mut Criterion) {
    use resume::integration::claude::{ClaudeRoot, discover};

    let mut group = c.benchmark_group("claude_discovery");
    group.sample_size(10);

    for &(files, avg_lines, big_files) in &[(200usize, 50usize, 0usize), (200, 50, 2)] {
        let tmp = tempfile::tempdir().expect("tempdir");
        fixtures::claude_project_tree(tmp.path(), files, avg_lines, big_files, 40);
        let root = ClaudeRoot {
            effective_root: tmp.path().to_path_buf(),
            nondefault: false,
        };

        let sanity = discover(&root).unwrap();
        assert_eq!(
            sanity.sessions.len(),
            files + big_files,
            "claude_discovery fixture sanity check"
        );
        assert!(
            sanity.diagnostics.is_empty(),
            "claude_discovery fixture produced unexpected diagnostics: {:?}",
            sanity.diagnostics
        );

        let bench_name = if big_files == 0 {
            format!("uniform_{files}files_{avg_lines}lines")
        } else {
            format!("uniform_{files}files_{avg_lines}lines_plus_{big_files}x40MB")
        };
        group.bench_function(bench_name, |b| {
            b.iter(|| {
                let outcome = discover(&root).unwrap();
                black_box(outcome.sessions.len());
            });
        });
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_git_scope,
    bench_codex_discovery,
    bench_pi_discovery,
    bench_omp_discovery,
    bench_claude_discovery
);
criterion_main!(benches);
