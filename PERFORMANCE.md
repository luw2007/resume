# Performance benchmarks

`resume` discovery was observed to take 20-40s wall-clock on `resume --json --up all`
against a real, long-lived `~/.codex/sessions` (thousands of rollout files, hundreds of
distinct Workspaces), and `Ctrl+C`/`Ctrl+D` felt slow because discovery held the process
for that entire window before the picker (or `--list`/`--json` exit) became responsive.
This document explains the benchmark suite added to measure and guard against that class
of regression, and the two root causes it was built to isolate.

## Running

```sh
cargo bench --bench discovery              # both groups
cargo bench --bench discovery -- git_scope       # one group only
cargo bench --bench discovery -- codex_discovery
```

No real Session data is read or required; every benchmark generates a synthetic fixture
in a fresh `TempDir` (see `benches/fixtures.rs`) and discards it afterward.

## What each group measures

### `git_scope`

`Scope::contains_workspace` in `ScopeMode::Git` (the default Scope inside a Git
repository) previously spawned a `git rev-parse --git-common-dir` subprocess on **every**
call, even when many Sessions share the same Workspace -- the common case for a
long-lived agent history under one or a few projects. Confirmed against a real
`~/.codex/sessions` with 3544 rollout files mapping to only 546 unique `cwd` values: 100%
redundant subprocess spawns for the other ~3000 calls. Worse, the subprocess was spawned
even in `ScopeMode::Up`/`Down`/`Exact` (e.g. `resume --up all`), where `contains()` never
reads the result at all.

Fixed by:

1. Only computing `git_common_dir` in `ScopeMode::Git` -- `contains()` never reads it in
   any other mode (`Up`/`Down`/`Exact`), so those modes now skip the subprocess spawn
   entirely.
2. Memoizing the result per canonical path in a `Scope`-owned `Mutex<HashMap<..>>` (the
   `Scope` is shared via `Arc` across per-agent discovery threads, so a `RefCell` is not
   sound here), per `docs/product-design.md` section 4: "Query each normalized Workspace
   at most once ... Cache only for the current process."

Measured effect (manual A/B comparison against a real Git repo, 1000 calls across 20
unique Workspace paths): **5.6ms/call unpatched -> ~0.02ms/call patched at scale**
(single-digit-microsecond cache hits after the first call per unique path, amortizing a
one-time ~5ms subprocess cost per distinct Workspace instead of paying it on every
Session). On a real `~/.codex/sessions` (`-a codex` only, same machine/data,
before/after): **~28s -> ~7-11s wall-clock**.

### `codex_discovery`

Codex's `discover_with_filter[_enriched]` previously read and fully JSON-parsed every line
of every rollout file (bounded only by the generous 512 MiB/file safety limit) purely to
locate the `session_meta` record and derive a title from user messages -- even for outlier
files tens of megabytes large. `docs/product-design.md` section 3 specifies title
derivation should use "at most a 1 MiB bounded early read," which the implementation now
honors: `parse_rollout_file` first attempts a bounded (1 MiB) read; if `session_meta` is
found within it (the normal case -- it is the first record in every real Codex rollout),
discovery proceeds from that fast-path read alone. Only if `session_meta` is not found
within the bound (an anomalous shape) does it fall back to a full read at the
caller-supplied safety ceiling, so correctness is preserved for every input shape.

The `uniform_*files_*lines` vs `uniform_*_plus_NxMB` benchmark pair makes the fix's effect
visible: the large-outlier-file variant dropped from ~226ms to ~19ms (91.7% improvement,
criterion-confirmed significant), while the small-file baseline is unchanged. On real
`~/.codex/sessions` data (`-a codex` only, same machine/data, before/after this fix):
**~11s -> ~5-10s wall-clock** (on top of the `git_scope` fix's own ~28s -> ~11s). Output
was verified byte-identical (same sessions, titles, workspaces, support/activity/risk,
zero errors) between the unfixed and fixed binaries against the same real data.

## Fixtures

`benches/fixtures.rs::codex_tree` generates a synthetic `CODEX_HOME`-shaped directory:
`sessions/YYYY/MM/DD/rollout-*.jsonl`, each with a valid `session_meta` header and a
configurable number of synthetic `event_msg` records, plus optional large outlier files
padded to a target size with repeated filler content. Workspaces fan in across
`sqrt(files)`-ish distinct paths (bounded to at most 20) to reproduce the real-world
many-Sessions-per-Workspace shape that made the `git rev-parse` overhead so costly. No
real Session content, identifiers, or paths are used.

`benches/fixtures.rs::init_git_repo` creates a real (not mocked) temporary Git repository
so the `git_scope` group exercises the actual subprocess path, not a stub.

## Interpreting results

`criterion` compares each run against the previous run's saved baseline
(`target/criterion/`, gitignored) and reports a `change:` delta with a significance test.
A regression in either group's reported time is the primary signal to investigate before
merging a change that touches `Scope::contains_workspace` or Codex/Pi/Claude/OMP rollout
parsing.

## Known remaining gaps (not yet fixed)

- **Claude Code and Pi/OMP discovery are not yet covered by this benchmark suite.** Only
  Codex has a dedicated `codex_discovery` group; the other three integrations parse
  JSONL similarly (Pi/OMP already cap `max_records`, but not `max_file_bytes`, so a single
  pathologically large Pi/OMP/Claude transcript could show the same file-size sensitivity
  the pre-fix Codex benchmark demonstrated). Extending `benches/fixtures.rs` with
  Pi/OMP/Claude-shaped synthetic trees and adding matching benchmark groups is the natural
  next step before assuming this class of regression is fully guarded.
- **The Codex bounded early read is a fixed 1 MiB, not adaptive.** If a future rollout
  format regularly needs more than 1 MiB before the first user message (unlikely given
  current evidence, but not proven impossible), the fallback path pays the full-read cost
  every time rather than a slightly larger bounded read. No evidence currently motivates
  tuning this, but it is the parameter to revisit if a future fixture shows the fallback
  path triggering unexpectedly often on real data.
