# Performance benchmarks

`resume` discovery was observed to take 20-40s wall-clock on `resume --json --up all`
against a real, long-lived `~/.codex/sessions` (thousands of rollout files, hundreds of
distinct Workspaces), and `Ctrl+C`/`Ctrl+D` felt slow because discovery held the process
for that entire window before the picker (or `--list`/`--json` exit) became responsive.
This document explains the benchmark suite added to measure and guard against that class
of regression: two root causes fixed for Codex (`git_scope`, `codex_discovery`), and
per-integration file-size sensitivity tracked (not yet fixed) for Pi, OMP, and Claude
(`pi_discovery`, `omp_discovery`, `claude_discovery`).

## Running

```sh
cargo bench --bench discovery              # all groups
cargo bench --bench discovery -- git_scope       # one group only
cargo bench --bench discovery -- codex_discovery
cargo bench --bench discovery -- pi_discovery
cargo bench --bench discovery -- omp_discovery
cargo bench --bench discovery -- claude_discovery
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

### `pi_discovery` / `omp_discovery`

Pi and OMP discovery (`src/integration/pi/discover.rs::extract_session`,
`src/integration/omp/format.rs::extract_session`) need "latest wins" semantics for the
display title (`session_info.name` for Pi, `title_change` records for OMP) and for the
activity timestamp (`latest_message_time`, computed by scanning every user message in the
file). Both therefore **cannot** adopt Codex's "stop at the first user message"
bounded-early-read fix without further correctness research: an early exit could silently
return a stale title or an out-of-date activity time instead of the true latest value.
Checked against a real `~/.pi` corpus (581 transcripts): every file's first user message
appears very early (median line 4, max line 25 -- so an early read *would* find a user
message quickly), but **135 of 581 files (23%) contain more than one user message**,
meaning `latest_message_time` genuinely differs from the first message's time for nearly
a quarter of real Sessions -- confirming the "stop at first" fix would produce a wrong
(stale) activity time for a real, non-negligible fraction of Sessions, not just a
theoretical edge case. (Zero of the 581 real files contained a `session_info` record at
all, so the title side of this risk was not directly measurable against this corpus, but
the activity-time finding alone is sufficient to rule out a naive early-exit fix.) These
groups exist to make Pi/OMP's file-size sensitivity visible and trackable, not to
demonstrate an applied fix.

Measured (same synthetic scale as `codex_discovery`, one process on one machine): both
Pi and OMP go from ~22ms (small files only) to ~78-82ms with two 40 MiB outlier files
mixed in (~3.6x slower). On real data, this has not yet caused user-visible slowness:
a real `~/.pi` (581 files, 170 MB, max 7 MB) and `~/.claude` (876 files, 422 MB, max 21
MB) both discover in ~1-1.5s steady-state on the machine these benchmarks were authored
on, far below Codex's pre-fix, much-larger corpus (3544 files, 2.9 GB). Revisit if a
future real-world `~/.pi` or `~/.omp` corpus grows to Codex's pre-fix scale.

### `claude_discovery`

Claude's field resolution (`src/integration/claude/format.rs::interpret_record`) is
"first non-empty wins" for `agent_name`/`ai_title`, which is structurally
early-exit-friendly -- but neither field is guaranteed to appear near the start of a real
transcript (unlike Codex's `session_meta`, which is always the first record). Checked
against a real `~/.claude/projects` corpus (876 transcripts): `agent-name`/`agentName`
appears in 575 files at a **median byte offset of ~209 KB but a 90th-percentile offset of
~1.28 MB** (max ~3.9 MB); `ai-title`/`aiTitle` appears in 4703 files at a median *line*
position of 163 but a max line position of 7520. A naive 1 MiB-bounded early read (as
applied to Codex) would therefore **silently produce a worse title for more than 10% of
real transcripts with an `agent-name`** by missing the field entirely and falling through
to a less-authoritative title source -- a real, quantified correctness risk, not a
theoretical one. This group measures the existing full-file-parse behavior; the concrete
next step for a Claude fix is a strategy that does not trade this correctness away (e.g.
a larger or adaptive bound informed by the observed distribution above, or reading the
tail of the file for late-appearing fields instead of only the head).

Measured (same synthetic scale): ~17ms (small files) to ~194ms with two 40 MiB outliers
(~11.4x slower) -- the most file-size-sensitive of the three per-benchmark, though real
`~/.claude` data has not shown this in practice at the corpus sizes observed so far (see
`pi_discovery`/`omp_discovery` above for the equivalent real-corpus comparison).

## Fixtures

`benches/fixtures.rs::codex_tree` generates a synthetic `CODEX_HOME`-shaped directory:
`sessions/YYYY/MM/DD/rollout-*.jsonl`, each with a valid `session_meta` header and a
configurable number of synthetic `event_msg` records, plus optional large outlier files
padded to a target size with repeated filler content. Workspaces fan in across
`sqrt(files)`-ish distinct paths (bounded to at most 20) to reproduce the real-world
many-Sessions-per-Workspace shape that made the `git rev-parse` overhead so costly. No
real Session content, identifiers, or paths are used.

`benches/fixtures.rs::grouped_session_tree` builds the equivalent Pi/OMP-shaped grouped
tree (`<session_root>/<workspace-key>/session-*.jsonl`, v3 header + user messages), and
`benches/fixtures.rs::claude_project_tree` builds the equivalent Claude-shaped
`<claude_root>/projects/<workspace-key>/<uuid>.jsonl` tree with filename/`sessionId`
UUIDs that agree (Claude's exact-identity contract). All three share the same
`files`/`avg_lines`/`big_files`/`big_file_mb` parameters and `sqrt(files)`-ish Workspace
fan-out as `codex_tree`, so their benchmark results are directly comparable across
integrations.

`benches/fixtures.rs::init_git_repo` creates a real (not mocked) temporary Git repository
so the `git_scope` group exercises the actual subprocess path, not a stub.

Every `pi_discovery`/`omp_discovery`/`claude_discovery` benchmark function asserts the
fixture's expected Session count once (untimed, before entering the timed loop) so a
future fixture-generator bug fails loudly instead of silently benchmarking a no-op. One
such bug was caught while building these groups: `Scope::new` requires its `base` path
canonicalized (matching what `app::build_scope` does for the real CLI via
`scope::canonical_base`) -- on macOS, `TMPDIR` resolves through a `/var` ->
`/private/var` symlink, so an uncanonicalized `tmp.path()` silently failed every
`ScopeMode::Down`/`Up` `strip_prefix`/`ancestors` check and excluded every Session as
out-of-scope.

## Interpreting results

`criterion` compares each run against the previous run's saved baseline
(`target/criterion/`, gitignored) and reports a `change:` delta with a significance test.
A regression in either group's reported time is the primary signal to investigate before
merging a change that touches `Scope::contains_workspace` or Codex/Pi/Claude/OMP rollout
parsing.

## Known remaining gaps (not yet fixed)

- **Pi and OMP discovery still scale with total rollout bytes, not Session count, and a
  naive Codex-style early-exit fix would be incorrect for real data**, not just
  theoretically risky: a real `~/.pi` corpus check found 23% of files (135/581) have more
  than one user message, so `latest_message_time` genuinely requires scanning to the true
  last occurrence for a meaningful fraction of real Sessions (see `pi_discovery`/
  `omp_discovery` above for the full research writeup). A correct fix needs a strategy
  that finds the *latest* match within a bound (e.g. reading the file in reverse, or a
  two-pass scan bounded by total bytes rather than position) rather than stopping at the
  first match. Not yet a user-visible problem at real-world corpus sizes observed so far,
  but worth revisiting if a real `~/.pi` or `~/.omp` grows toward Codex's pre-fix scale
  (thousands of files, gigabytes).
- **Claude discovery has the same scaling problem, is the most file-size-sensitive of the
  three in this benchmark's synthetic scale (~11.4x slower with two 40 MiB outliers vs
  ~3.6x for Pi/OMP), and a naive fix is similarly ruled out by real data**: a real
  `~/.claude` corpus check found `agent-name`/`agentName` needs more than 1 MiB of file
  content to reach in over 10% of the 575 files that have it (90th percentile ~1.28 MB,
  max ~3.9 MB), so a 1 MiB-bounded early read (Codex's exact fix) would silently produce
  a worse title for those files by missing the field and falling through to a
  less-authoritative source. Despite this, Claude's "first non-empty wins" semantics are
  structurally more early-exit-friendly than Pi/OMP's "latest wins" once a correctly-sized
  or adaptive bound is found -- the concrete next step is picking a bound (or a
  two-pass/tail-read strategy) informed by this measured distribution, not ruling out an
  early-exit approach entirely.
- **The Codex bounded early read is a fixed 1 MiB, not adaptive.** If a future rollout
  format regularly needs more than 1 MiB before the first user message (unlikely given
  current evidence, but not proven impossible), the fallback path pays the full-read cost
  every time rather than a slightly larger bounded read. No evidence currently motivates
  tuning this, but it is the parameter to revisit if a future fixture shows the fallback
  path triggering unexpectedly often on real data.
- **Preview-time full-transcript parsing is not yet benchmarked.** The current picker's
  Preview only renders Session metadata (status/agent/time/title/workspace); it does not
  yet render a full transcript for any integration, so there is no real code path to
  benchmark for "parse the whole Session on demand when the user previews/selects it."
  Add a benchmark here once Preview grows a full-transcript rendering path.
