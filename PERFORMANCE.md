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

Two further real-corpus findings on `contains_workspace`, from testing `-a omp` inside
this repository's own default (Git) Scope against the repo owner's real `~/.omp`
corpus (1353 files, 1.2 GB, 125 distinct recorded `cwd` values):

1. **`ScopeMode::Git` still spawned one `git rev-parse` per distinct Workspace, even for
   Workspaces that could never match.** Of the 125 distinct `cwd` values in the real
   corpus, only 1 shared a path prefix with this repository's worktree. `contains()`'s
   Git branch never returns `true` unless `real_path` also starts with one of the
   repository's worktrees -- `git_common_dir` only ever *narrows* a match (excludes a
   distinct nested repository), it never *widens* one. So checking the (subprocess-free)
   `starts_with` prefix match first, before spawning `git rev-parse` for
   `git_common_dir`, is a pure reordering with no correctness change: a Workspace that
   fails the prefix check can never match regardless of `git_common_dir`, so its
   subprocess spawn is now skipped entirely. This removed the spawn for ~99% (124/125) of
   distinct Workspaces in the measured corpus. Fixed in `Scope::contains_workspace`.
2. **The default Git Scope resolved the current repository's *entire* linked-worktree
   list (`git worktree list --porcelain`, a second subprocess call) even though almost no
   real invocation cares about Sessions from a *different* linked worktree of the same
   repository.** `docs/product-design.md` section 4 now documents the current worktree as
   the default Scope, with a new `--all-worktrees` flag to opt back into the previous
   "every linked worktree" behavior. The narrowed default resolves the Git common
   directory and current worktree together in a single `git rev-parse
   --git-common-dir --show-toplevel` call and never spawns `git worktree list` at all,
   removing that second subprocess entirely from the default path.

Combined effect on `-a omp` inside this repository's own default Scope (real corpus
above, steady state, `git rev-parse`/`git worktree list` subprocess overhead only --
JSON-parsing cost below is unaffected by either fix): **default-Scope `omp::discover()`
~3.4-3.6s -> ~2.6-2.9s** (matching the non-Git `Exact`-Scope cost, since the default path
no longer pays any Git-Scope-specific subprocess overhead beyond the one combined
`git rev-parse` call already required to resolve the current worktree itself).

### `codex_discovery`

Codex's `discover_with_filter[_enriched]` previously read and fully JSON-parsed every line
of every rollout file (bounded only by the generous 512 MiB/file safety limit) purely to
locate the `session_meta` record and derive a title from user messages -- even for outlier
files tens of megabytes large. `docs/product-design.md` section 3 specifies title
derivation should use "at most a 1 MiB bounded early read," which the implementation now
honors: `parse_rollout_file` first attempts a bounded read; if `session_meta` is found
within it (the normal case -- it is the first record in every real Codex rollout),
discovery proceeds from that fast-path read alone. Only if `session_meta` is not found
within the bound (an anomalous shape) does it fall back to a full read at the
caller-supplied safety ceiling, so correctness is preserved for every input shape.

The bound started at 1 MiB (documented ceiling) and was later tightened to 64 KiB after
measuring the *actual* real-corpus distribution: against 3546 real rollouts (~2.9 GB), a
64 KiB budget finds `session_meta` in exactly the same set of files a 1 MiB budget did
(zero additional fallbacks), and produces a byte-identical first-user-message title for
all 3580 discoverable sessions when directly compared against a full read -- the 1 MiB
ceiling was never actually needed by any real file observed; it was headroom, not a
requirement. `docs/product-design.md`'s "at most a 1 MiB" is a ceiling, not a floor, so
64 KiB remains compliant.

The `uniform_*files_*lines` vs `uniform_*_plus_NxMB` benchmark pair makes the fix's effect
visible: at 1 MiB, the large-outlier-file variant dropped from ~226ms to ~19ms (91.7%
improvement vs the pre-fix unbounded read); at 64 KiB, it dropped further to ~18ms,
statistically indistinguishable from the small-file baseline (~17ms) -- the large-file
penalty is fully eliminated at this bound for the synthetic fixture, not just reduced.

On real `~/.codex/sessions` data (`-a codex` only, same machine/data): the 1 MiB fix
brought discovery from ~28s (pre-any-fix, dominated by the separately-fixed `git_scope`
issue) to ~11s wall-clock; the 64 KiB refinement brought it further to **~1.0-1.6s
steady-state** (measured via a direct same-session A/B: two isolated worktrees built from
the same commit except this one constant, run back-to-back against identical real data).
Output was verified byte-identical (same sessions, titles, workspaces,
support/activity/risk, zero errors) between the 1 MiB and 64 KiB binaries against the
same real data, both via a full-corpus per-file comparison (3580 discoverable sessions,
0 title/session_meta-presence mismatches) and via the assembled CLI's complete `--json`
output (10 sessions, byte-identical across `agent`/`profile`/`id`/`title`/`workspace`/
`support`/`activity`/`risk` for every session, zero errors both before and after).

**Workspace gate (out-of-scope fast path).** Codex has no Workspace-encoded
directory layout to prune (its store is date-partitioned, `sessions/YYYY/MM/DD/`),
so the directory-name prefilter applied to Pi/OMP/Claude does not transfer. The
equivalent fix is a per-file first-record gate (`parse_rollout_file_gated` with a
`WorkspaceGate`): a `max_records: 1` read resolves `session_meta.payload.cwd` --
the shared reader stops at the first parsed record -- and an out-of-Scope `cwd`
skips the title-derivation read entirely. A fixed small byte budget would NOT
work here: real first lines are large (median ~4 KiB, p99 ~22 KiB across 3582
real rollouts -- the payload embeds instructions), so a 4 KiB budget truncates
half the corpus and the gate always falls through; the record cap is what makes
the gate cheap regardless of line size. Measured on the real corpus
(interleaved min-of-4, same machine/data): **`--json --agent codex` 2.64s ->
1.66s (~1.6x)**, byte-identical `--json` output. The remaining ~1.7s is the
per-file floor -- open + read + `serde_json::Value`-parse of the fat first
record, which the gate itself must pay to learn `cwd` -- so any further gain
requires either a lighter typed parse of just `payload.cwd` for the gate
(skipping `Value` tree construction), or not opening files at all via the
`state_5.sqlite` `threads` table (`rollout_path`/`cwd`/`title`/`updated_at`;
verified complete and synchronous against this corpus: 3579/3582 rows,
newest rollout present, backfill complete). The DB route is gated behind the
optional `codex-sqlite` feature and would soften the documented "deleting the
DB never changes discovery" contract, so it is recorded here as the next
step's tradeoff, not implemented.

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

**OMP re-checked directly** (not extrapolated from Pi) against a real `~/.omp/agent`
corpus of 1351 transcripts, ~1.03 GB, 227 files over 1 MiB, max 20.8 MB -- the largest
real corpus of the three non-Codex integrations checked so far, and large enough that
OMP discovery is genuinely user-visible slow at this scale (~2.4-3.9s steady-state, not
the sub-2s seen on the smaller Pi/Claude corpora below). With the correct field
(`record.message.role == "user"`, not a bare `type` field), **551 of 1351 files (40.8%)
contain more than one user message** -- an even higher rate than Pi's 23%, further
confirming "stop at first" is unsafe for OMP on real data. Checked one more angle before
concluding no safe shortcut exists: for files with a user message, the *last* one's
position averages 27.8% into the file by line count (median), but the 90th percentile is
94.5% of the way through -- so even "read only the last N% of the file" is not reliably
sufficient either; a real fix needs the file's full content available to find the true
latest match, not a bounded window at either end. Profiled where OMP's ~2.4s steady-state
actually goes: raw `jsonl::read_file_confined` alone (no OMP-specific extraction) over
all 1351 files takes the same ~2.4s discover() does, confirming the cost is the shared
JSON-parsing layer (`serde_json::from_slice::<Value>`, ~65-75% of the total; the shared
`nesting_depth` bound check is a further ~5-10%), not OMP-specific waste -- there is
currently no safe, low-risk optimization available for OMP without either widening the
early-read window well past what any bound would help (average file needs to reach 28%+
of its own length) or changing the underlying parse strategy for all four integrations at
once (a materially larger architectural change, out of scope here).

Measured (same synthetic scale as `codex_discovery`, one process on one machine): both
Pi and OMP go from ~22ms (small files only) to ~78-82ms with two 40 MiB outlier files
mixed in (~3.6x slower). On real data, Pi and Claude have not yet caused user-visible
slowness at the corpus sizes checked (`~/.pi`: 581 files, 170 MB, max 7 MB; `~/.claude`:
876 files, 422 MB, max 21 MB; both ~1-1.5s steady-state) -- but **OMP already has**, at a
real corpus roughly 6x larger by total bytes than the Pi corpus checked (1351 files,
~1.03 GB). Revisit if a future real-world `~/.pi` corpus grows to a similar scale, or if
OMP's real corpus continues growing toward Codex's pre-fix scale (3544 files, 2.9 GB).

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

- **Pi, OMP and Claude discovery now prune whole out-of-Scope Workspace
  directories by their encoded directory name before reading any file.** All
  three integrations group
  Sessions under a directory whose name lossily encodes the Workspace path: Pi
  uses `-{abs path with '/' -> '-'}-`, OMP uses
  the home-relative form under `$HOME` and the absolute form otherwise, and
  Claude maps every non-alphanumeric character (not just `/`) to `-`. Since a
  literal `-` in a path component is indistinguishable from a separator (and
  Claude collapses `.`, `_`, etc. as well), the
  prefilter (`Scope::may_contain_session_dir`, normalizing BOTH sides to the
  coarsest key: every non-ASCII-alphanumeric character -> `-`) is deliberately
  lossy-conservative:
  it only skips a dash-prefixed directory when *no* decoding of its name could be
  in Scope; ambiguity always keeps the directory, and the header `cwd` remains
  authoritative for every file actually read. Custom session roots (flat layouts,
  where directory names carry no encoding) are never pruned. Measured on real
  corpora inside this repository's default Scope, back-to-back binaries on
  identical data: **OMP `--json --agent omp` 4.85s -> 0.26s; Pi 1.72s -> 0.06s;
  Claude 0.92s -> 0.01s** (89 project directories, none in this repository's
  Scope), with identical discovered session sets in every case, including a
  positive-hit check from `$HOME` (both binaries: same 1 session).
  The prior finding still holds for *in-Scope* bytes: for files inside kept
  directories, "latest wins" semantics require a full scan (23% of real Pi files
  and 40.8% of real OMP files have more than one user message), and the shared
  `serde_json::Value` parse layer dominates that remaining cost. A lighter-weight
  streaming extraction remains the only further optimization for corpora whose
  Sessions are mostly in Scope (e.g. `--up all`, where the prefilter keeps
  everything).
- **For in-Scope Claude directories, per-file parsing remains the most
  file-size-sensitive of the
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
- **The Codex bounded early read is a fixed 64 KiB (tightened from an initial 1 MiB after
  real-corpus measurement showed 1 MiB was never actually needed), not adaptive.** If a
  future rollout format regularly needs more than 64 KiB before both `session_meta` and
  the first user message (unlikely given current evidence -- 0 of 3546 real files needed
  more, and even 1 MiB never needed to be used in practice -- but not proven impossible),
  the fallback path pays the full-read cost every time rather than a slightly larger
  bounded read. No evidence currently motivates tuning this further, but it is the
  parameter to revisit if a future fixture shows the fallback path triggering
  unexpectedly often on real data.
- **Preview-time full-transcript parsing is not yet benchmarked.** The current picker's
  Preview only renders Session metadata (status/agent/time/title/workspace); it does not
  yet render a full transcript for any integration, so there is no real code path to
  benchmark for "parse the whole Session on demand when the user previews/selects it."
  Add a benchmark here once Preview grows a full-transcript rendering path.
