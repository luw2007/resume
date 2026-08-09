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

Codex's `discover_with_filter[_enriched]` reads and fully JSON-parses every line of every
rollout file (bounded only by the generous 512 MiB/file safety limit) purely to locate the
`session_meta` record and derive a title from user messages -- even for outlier files tens
of megabytes large. `docs/product-design.md` section 3 specifies title derivation should
use "at most a 1 MiB bounded early read," which the current implementation does not honor
for Codex. The `uniform_*files_*lines` vs `uniform_*_plus_NxMB` benchmark pair makes this
file-size sensitivity visible: a handful of large outlier files can dominate total
discovery time disproportionately to their session count. This group is a regression
guard for the current (unbounded) behavior and a baseline to compare against once a
bounded-read fix lands; it does not yet assert an upper bound, since the current behavior
is the thing still being fixed.

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

## Known remaining gap (not yet fixed)

The `codex_discovery` large-file variant demonstrates that Codex discovery time still
scales with total bytes read across all rollout files, not with the number of Sessions or
the amount of Preview-relevant content. A future fix should bound the read used for title
derivation to the documented 1 MiB early-read budget (falling back to full parsing only
inside Preview, on demand for the selected Session) rather than fully parsing every
rollout up front during discovery.
