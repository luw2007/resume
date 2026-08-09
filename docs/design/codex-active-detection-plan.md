# Codex Active Detection — Design Plan

Status: proposed
Scope: `src/integration/codex/`, `src/app.rs` wiring, module split of `codex/mod.rs` + `codex/sqlite.rs`
Deliverable of this document: design only. No `.rs` file other than the new
`src/integration/codex/activity.rs` signature draft is touched by this change.

---

## 0. Problem

`README.md` (Support List) states:

> The assembled app currently supplies no live-correlation evidence to Pi or OMP,
> and Codex/Claude Sessions therefore normally report Unknown.

The Codex module doc (`src/integration/codex/mod.rs:33-38`) already *specifies* an
activity contract — positive evidence only, a live process holding an open file
descriptor on the exact rollout — but nothing implements it. Concretely:

- `codex::build_session` hardcodes `activity: ActivityStatus::Unknown`
  (`src/integration/codex/mod.rs:586`).
- `src/app.rs` passes `None` for evidence to Pi (`src/app.rs:407`) and OMP
  (`src/app.rs:531`), so the two integrations that *do* have working
  `activity_status` + evidence types never receive input.
- There is no probe of any kind in the binary. `grep Command::new src/` yields only
  `git` invocations in `src/scope.rs:174,208,221` and the `exec()` at
  `src/launch.rs:196`.

So the gap is not "detection is wrong", it is "detection was never wired". This
document decides the mechanism, the wiring contract, the O(1) subprocess budget,
the two correctness edge cases, and the module split that gives the new logic a
home.

Prior art to mirror (both fully implemented, both starved of input):

| | producer | evidence type | match predicate |
|---|---|---|---|
| Pi | `pi::activity_status` `src/integration/pi.rs:563-572` | `SessionControlEvidence` `src/integration/pi.rs:580-587` | `src/integration/pi.rs:589-602` |
| OMP | `omp::activity_status` `src/integration/omp/mod.rs:837-847` | `ActivityEvidence` `src/integration/omp/mod.rs:852-863` | `src/integration/omp/mod.rs:867-879` |

Both return `Active { observed_at }` only on a positive match and `Unknown`
otherwise — **never `Inactive`**. Codex adopts the same shape.

---

## 1. Detection signal

**Decision: one `std::process::Command::new("lsof")` call per discovery run,
selecting processes by command-name prefix, in NUL-delimited field mode.**

No new dependency: `std::process::Command` only, matching the existing
`.args([OsStr…]).output()` convention in `src/scope.rs:174` and `src/scope.rs:208`.
No shell is spawned anywhere in this repo and none is spawned here.

### Exact argv

```
lsof  -n  -P  -w  -S 2  -F0pcfnDi  -c codex
```

| flag | why |
|---|---|
| `-n` | no DNS reverse lookup (latency only) |
| `-P` | no port-name lookup (latency only) |
| `-w` | suppress per-process warnings on stderr; good rows still print |
| `-S 2` | 2s cap per kernel `stat`/`readlink`, so a wedged NFS mount cannot stall discovery (2 is lsof's minimum accepted value) |
| `-F0pcfnDi` | field mode, NUL-terminated fields, newline-terminated sets; fields = **p**id, **c**ommand, **f**d, **n**ame, **D**evice, **i**node |
| `-c codex` | prefix match on command name |

Every argument is a fixed literal. **No session-derived value is ever
interpolated into argv.** Use `-c codex`, not `-c /^codex$/` — the regex form is
not portable to older `lsof` builds.

### Output format assumptions

Verified on macOS 26.5 / lsof 4.91:

```
p14339\0ccodex\0\n
fcwd\0D0x100000f\0i273524200\0n/Users/me/ai/proj\0\n
f11\0D0x100000f\0i289644924\0n/Users/me/.codex/sessions/2026/07/27/rollout-….jsonl\0\n
```

A `p` set opens a process context; every following `f` set is one descriptor
until the next `p` set. Only `f` sets whose `n` value passes
`roots::is_rollout_filename` (today `src/integration/codex/mod.rs:362`) on its
file name are retained. `D` is `0x`-prefixed hex, `i` is decimal; verified that
`D0x100000f` / `i306775406` equals `st_dev=16777231` / `st_ino=306775406` from
`stat(2)`.

### Fallback ladder

1. `launch::command_available(OsStr::new("lsof"))` (`src/launch.rs:94`) is false →
2. `#[cfg(target_os = "linux")]` walk `/proc/*/fd` with pure `std::fs`
   (`read_dir` + `read_link` + `metadata` for dev/ino, `/proc/<pid>/comm` for the
   command name). **Zero subprocesses.** Unreadable PIDs are `Err` and are
   skipped individually →
3. otherwise `ActivitySnapshot::empty()`; every session stays `Unknown`, never
   `Inactive`.

### When the tool is missing

**No diagnostic is emitted.** This mirrors `SqliteOutcome::Absent` producing zero
diagnostics at `src/app.rs:466`. A diagnostic is emitted only for a genuine
malfunction (spawn error, or parsable-but-partial output) — see §4.

This matters for tests: `tests/step9_app.rs:46-48` runs with `.env_clear()` and
`PATH=$home/bin`, so `lsof` is *always* unavailable there. The integration tests
therefore stay deterministic and stderr-clean, and Codex activity stays `Unknown`
in them.

### Rejected alternatives

| alternative | why rejected |
|---|---|
| `lsof -- <path>` or `fuser` per session | O(N) spawns — exactly the budget §2 forbids |
| unfiltered `lsof -F0pcfnDi` (no `-c`) | measured 61,708 lines / 250 ms vs 444 lines / 43 ms, and floods stderr with permission errors from foreign processes for no gain |
| `pgrep -x codex` then `lsof -p <list>` | two spawns instead of one, and `lsof -p` with one bad PID exits nonzero (verified `EXIT=1` for `-p 1,99998,99999`) — precisely the fragility §4 wants gone |
| `ps` / `/proc/<pid>/cmdline` argv scraping | Codex does not pass the rollout path on argv; it opens it. No positive evidence available |
| lock files / `flock` probing | Codex advertises no lock protocol; would be inventing a contract |
| `/proc` as the *primary* path on Linux | keeps one parser to test on both platforms; macOS has no `/proc`, and `lsof` always ships with macOS |

---

## 2. O(1) subprocess budget

### The sequencing tension, and its resolution

The probe must run **before** the per-agent threads are spawned, because
`run_interactive` streams each record into skim the moment it is built
(`src/app.rs:224-227`) — there is no post-join hook in the interactive path
(only `discover_all` has one, at `src/app.rs:188`). At that point **no session
list exists yet**.

Resolution: the probe is **process-first, not path-first**. It enumerates live
Codex processes and their open descriptors, and builds a `path → evidence` map.
Each integration then *looks itself up*. A path-first design would need the
session list and would inevitably tempt an O(N) shape; a process-first design
cannot.

### Insertion

```rust
// src/app.rs, immediately after the scope_warning_diagnostic block
// (after line 162 in discover_all, after line 197 in run_interactive):
let (activity, activity_diagnostics) = if options.agents.iter().any(|a| a == codex::AGENT) {
    codex::activity::probe()
} else {
    (codex::activity::ActivitySnapshot::empty(), Vec::new())
};
state.errors.lock().unwrap().extend(activity_diagnostics);
let activity = Arc::new(activity);
```

Exactly one of `discover_all` / `run_interactive` runs per process (branch at
`src/app.rs:81`), so exactly one probe happens per invocation.

### Spawn count: 1, a hard constant

- **1** when `codex` ∈ `options.agents` and `lsof` is on `PATH`
- **0** when `codex` ∉ `options.agents` (e.g. `resume -a pi`)
- **0** when `lsof` is absent
- **0** on the Linux `/proc` fallback

It is never 2: the `-c codex` command-name selector makes a separate
PID-enumeration step unnecessary, so the "one call to list PIDs, one call to dump
FDs" shape is not needed at all.

### Where the map is built

Inside `activity::probe()`, from the single `Output::stdout` buffer, into
`ActivitySnapshot`'s three `HashMap`s. The snapshot is immutable and shared as
`Arc<ActivitySnapshot>` across the per-agent threads.

### Complexity table

N = discovered sessions, K = sessions actually held open by a live Codex process,
F = total FDs held by codex processes (≈440 lines for 5 processes, measured).

| resource | cost | scales with N? |
|---|---|---|
| process spawns | **1 (constant)** | no |
| probe wall time | ~43 ms measured (`-c codex`) | no |
| parse work | O(F) | no |
| per-session lookup, exact-path hit | 1 hash lookup, **0 syscalls** | O(N) hashes only |
| per-session lookup, symlink-skew path | 1 extra `stat`, and only when a basename candidate exists → at most K times | no (K ≪ N) |
| allocations | O(F) for entries + index; per-session lookup allocates nothing | no |

Note this is *strictly cheaper* than the existing precedent:
`pi::SessionControlEvidence::matches` (`src/integration/pi.rs:594-596`) and
`omp::ActivityEvidence::matches` (`src/integration/omp/mod.rs:870-872`) call
`Path::canonicalize()` on **every** session.

---

## 3. Symlinked-ancestor correctness

**Requirement:** if an ancestor directory of a session path is a symlink (classic
macOS `/tmp -> /private/tmp`), evidence must not be lost.

**Why it is a live hazard here:** `ParsedSession.rollout_path` is canonicalized
*best effort* at `src/integration/codex/mod.rs:459`:

```rust
path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
```

so it can silently fall back to the non-canonical form, while `lsof` always
reports the kernel-resolved form.

### Rule — index each observed open rollout under three keys

Resolve in this order:

1. **`by_path: HashMap<PathBuf, usize>`** — keyed by the path `lsof` printed,
   already fully symlink-resolved by the kernel. Hit ⇒ 0 syscalls, done.
2. **`by_identity: HashMap<(u64, u64), usize>`** — keyed by `(D, i)` parsed from
   the `D`/`i` fields. Consulted only if step 1 missed.
3. **`by_name: HashMap<OsString, Vec<usize>>`** — keyed by `Path::file_name()`.
   This is a **candidate selector only, never an answer**: a name hit triggers
   exactly one `fs::metadata` on the caller's path, and the candidate is accepted
   only if `(st_dev, st_ino)` equals the candidate's `(D, i)`.

Callers do **not** call `canonicalize()` — that is the whole point. The multi-key
index covers exactly the case where the `unwrap_or_else` at mod.rs:459 fell back.
Step 3 gating step 2 is what bounds the extra `stat` count to K rather than N.

### Worked example (`/tmp -> private/tmp`, real numbers)

`lsof` row:

```
f11 \0 D0x100000f \0 i306775406 \0 n/private/tmp/ch/sessions/2026/07/27/rollout-X.jsonl
```

Index built:

- `by_path["/private/tmp/ch/sessions/2026/07/27/rollout-X.jsonl"] = 0`
- `by_identity[(0x100000f /* 16777231 */, 306775406)] = 0`
- `by_name["rollout-X.jsonl"] = [0]`

| lookup | session carries | outcome |
|---|---|---|
| **A** canonicalization succeeded | `/private/tmp/ch/…/rollout-X.jsonl` | step 1 hits → **Active**, 0 syscalls |
| **B** canonicalization fell back | `/tmp/ch/…/rollout-X.jsonl` | step 1 misses; step 3 finds candidate `[0]`; one `metadata("/tmp/…")` returns `st_dev=16777231, st_ino=306775406` (verified identical for both path forms) → matches → **Active** |
| **C** different file, same basename (copy under a second `CODEX_HOME`) | `/other/root/…/rollout-X.jsonl` | step 1 misses; step 3 finds candidate; `(st_dev, st_ino)` differ → **Unknown**. No false positive |
| **D** file deleted between discovery and probe | — | `metadata` fails → `None` → **Unknown**, never a guess |

Basename alone is never sufficient; **device+inode is the arbiter.** Hard links to
the same rollout also resolve correctly (same inode) — desirable, since it is
literally the same open file.

---

## 4. Per-PID failure isolation

**Requirement:** if process introspection fails for one unrelated PID, evidence
for all other sessions in the same batch must not be wiped.

The design removes the failure mode *structurally* rather than tolerating it:

### 4.1 No PID list is ever passed

With `-c codex`, `lsof` selects processes itself and prints what it can read.
Verified: `lsof -p 1,99998,99999` exits **1** and prints nothing usable, whereas
`lsof -n -P -w -F0pcfnDi -c codex` exits **0** with full output even on a machine
with thousands of unreadable foreign processes.

### 4.2 The exit status is ignored entirely

`lsof` exits 1 both for "some selected process was unreadable" *and* for the
perfectly normal "nothing matched" (verified `EXIT=1` for `-c nosuchprocxyz`).
Branching on it would be wrong in both directions.

Only `Command::output()` returning `Err` (a genuine spawn failure) is treated as
a failure → `DIAG_PROBE_FAILED` + empty snapshot.

### 4.3 Parsing is per-record-set and skip-forward

Split `stdout` on `b'\n'` into sets, split each set on `b'\0'` into fields:

- a `p` set whose pid does not parse as `u32` → discard that **process context
  only**, `skipped += 1`, continue at the next `p` set; all previously
  accumulated entries are kept;
- an `f` set with no `n` field, or a non-UTF-8 name, or a name failing
  `is_rollout_filename` → skip **that one descriptor** only;
- a missing or unparsable `D` / `i` → **keep the entry**, index it by path and
  file name only. It degrades to path-exact matching; it is not dropped.

**The accumulator is never reset**, so one malformed region costs one region.

### 4.4 Diagnostics without discarding rows

If `entries` is non-empty **and** (`skipped > 0` or stderr is non-empty), emit
exactly one:

```rust
Diagnostic {
    category: DIAG_PROBE_PARTIAL,          // "codex_activity_probe_partial"
    count: skipped.max(1),
    verbose_path: None,
    verbose_chain: Some("lsof reported unreadable processes; evidence is partial".into()),
}
```

`count` is the aggregation unit summed by `aggregate_diagnostics`
(`src/app.rs:810`). Per `src/diagnostics.rs:1-5`, **no path or process name from
`lsof` ever appears in `verbose_chain`.**

`-w` keeps per-process `can't stat()` warnings off the user's terminal; they are
neither needed nor logged.

**Net rule: one bad PID costs that PID's descriptors and at most one diagnostic;
every other row survives.**

---

## 5. Wiring contract

### Decision: post-mutate `session.activity` in `discover_codex`

Chosen over changing `build_session`'s public signature.

Justification: it sits next to the two post-mutations already there
(`session.risk` at `src/app.rs:472`, `normalize_availability` at
`src/app.rs:474`), the rollout path is already recoverable via the existing
`codex_transcript_path` (`src/app.rs:548-560`), and it changes **zero** public
codex signatures — so `build_session` (`codex/mod.rs:552`),
`discover_with_filter` (`:157`), `discover_with_filter_enriched` (`:198`), and all
of `src/integration/codex/tests.rs` stay untouched.

Rejected: adding `snapshot: Option<&ActivitySnapshot>` to `build_session` would
force the parameter through both `discover_with_filter*` public signatures,
breaking every test call site and the sqlite tests, for no behavioral gain.

### Insertion points (6)

1. **`src/app.rs:162`** (end of the `discover_all` prelude) and
   **`src/app.rs:197`** (end of the `run_interactive` prelude): the `probe()`
   block from §2, producing `let activity = Arc::new(...)`.
2. **`src/app.rs:166-173`** and **`src/app.rs:204-214`** (the per-agent capture
   blocks): add `let activity = activity.clone();` alongside the existing
   `scope` / `state` / `cancel` clones.
3. **`src/app.rs:175`** and **`src/app.rs:216`** (inside each spawned closure):
   `discover_agent(&agent, &scope, &activity, since_cutoff, &cancel)`.
4. **`src/app.rs:326-331`** — changed signature:

```rust
fn discover_agent(
    agent: &str,
    scope: &Scope,
    activity: &codex::activity::ActivitySnapshot,
    since_cutoff: Option<std::time::SystemTime>,
    cancel: &CancelToken,
) -> AgentDiscovery
```

   and `src/app.rs:338` becomes `"codex" => discover_codex(scope, activity),`.
   The Pi / Claude / OMP arms are unchanged.
5. **`src/app.rs:446`** —
   `fn discover_codex(scope: &Scope, activity: &codex::activity::ActivitySnapshot) -> AgentDiscovery`.
6. **`src/app.rs:471-474`** — inside the `DiscoveredSession::Session(mut session)`
   arm, immediately after `normalize_availability(&mut session);`:

```rust
if let Some(rollout) = codex_transcript_path(&session) {
    session.activity = codex::activity::activity_status(&rollout, Some(activity));
}
```

`codex_transcript_path` splits `key.native_locator` on `"::"`
(`src/app.rs:548-560`); the locator is built as `"{id}::{rollout_path}"` at
`src/integration/codex/mod.rs:591-599` using the **canonicalized** path from
`mod.rs:459` — which is exactly the key §3 step 1 expects, so the common case is
a pure hash hit.

### `ActivitySnapshot` shape

Opaque struct owning `observed_at: SystemTime`, `entries: Vec<FdEvidence>`, and
the three indexes; `Clone + Debug`; constructed only by `probe` / `probe_with`;
queried only through `lookup`.

`observed_at` is captured once (`SystemTime::now()` at probe time) and reused
verbatim as `ActivityStatus::Active { observed_at }` for every session, so all
Active sessions in one run share one timestamp — deterministic, and honest about
when the observation actually happened.

### Pi and OMP: follow-up, not now

Their `None` arguments (`src/app.rs:407`, `src/app.rs:531`) stay. They need
entirely different producers (Pi's session-control registry, OMP's TTY
breadcrumbs) with their own correctness questions; retrofitting all three at once
triples the ordering/prompting blast radius in a single change and makes any
behavior regression impossible to bisect. **Land Codex, observe, then generalize.**
Claude has no activity seam at all and is out of scope.

### Test fallout

- `src/integration/codex/tests.rs`, `src/integration/codex/sqlite/tests.rs`: **none.**
- `src/app.rs` inline tests: only need the extra `discover_agent` argument if they
  call it — they do not today (they exercise `aggregate_diagnostics` /
  `status_label`).
- `tests/step9_app.rs`: unaffected, because `PATH` is scrubbed to `$home/bin`
  (`tests/step9_app.rs:48`) so `command_available` is false.
- `docs/qa/feature-inventory.csv:80`: must be rewritten — see §8 checklist item 8.

---

## 6. Module split

### Deviation, stated up front: no `format.rs`

`derive_title` (`src/integration/codex/mod.rs:617-633`) is one function with one
caller (`build_session`), and codex contains **zero** truncation / unicode-width
code (`unicode_width` appears only in `src/text.rs:284,294` and
`src/summary.rs:96,148`). A 12-line module for it is ceremony with no repo
precedent — `pi` is a single `pi.rs`; `claude` and `omp` are `mod.rs` +
`tests.rs`. `derive_title` goes to `discover.rs` beside its only caller. If
symmetry is wanted later it is a one-function move.

### Final assignment (`src/integration/codex/`)

| item (current line in `mod.rs`) | target | visibility |
|---|---|---|
| module doc comment `//!` :1-45 | `mod.rs` | — |
| `AGENT` :61 | `mod.rs` | `pub` (used as `codex::AGENT`, `src/app.rs:355`) |
| `ENV_CODEX_HOME` :64 | `roots.rs` | `pub` + `pub use` in mod.rs (tests.rs) |
| `SESSIONS_SUBDIR` :67, `ARCHIVED_SUBDIR` :70, `ROLLOUT_PREFIX` :73, `ROLLOUT_SUFFIX` :76 | `roots.rs` | private |
| `effective_root` :86 | `roots.rs` | `pub` + `pub use` (`src/app.rs:447`) |
| `dirs_home` :98 | `roots.rs` | `pub(super)` (only `tests.rs:582` outside roots) |
| `rollout_roots` :106, `RolloutRoot` :128, `RolloutKind` :138 | `roots.rs` | `pub` + `pub use` (tests.rs) |
| `is_rollout_filename` :362 | `roots.rs` | `pub(crate)` (discover.rs + activity.rs + tests.rs) |
| `TYPE_SESSION_META` :79, `MAX_USER_MESSAGES` :82 | `discover.rs` | private |
| `discover` :147, `discover_with_filter` :157, `discover_with_filter_enriched` :198 (+ local `enum Pending`) | `discover.rs` | `pub` + `pub use` |
| `DiscoveredSession` :299 + `impl` :311 | `discover.rs` | `pub` + `pub use` |
| `list_rollout_files` :326, `list_rollout_files_into` :337 | `discover.rs` | private |
| **`ParsedSession` :370-420**, `ImportMeta` :422 | `discover.rs` | `pub` + `pub use discover::{ImportMeta, ParsedSession};` |
| `parse_rollout_file` :430 | `discover.rs` | `pub` + `pub use` |
| `parse_rollout_records` :454 | `discover.rs` | `pub(crate)` — reached as `crate::integration::codex::discover::parse_rollout_records` |
| `find_session_meta` :541, `build_session` :552, `native_locator` :591, `canonicalize_workspace` :601, `derive_title` :617, `extract_*` :635-732, `push_dedup` :791, `fingerprint` :804, `extract_import` :828, `invalid` :902 | `discover.rs` | private (`build_session` `pub` + `pub use`, to preserve today's public API) |
| `resume_spec` :849, `is_default_root` :890 | `resume.rs` | `resume_spec` `pub` + `pub use` (`src/app.rs:475`); `is_default_root` private |
| `#[cfg(feature)] pub mod sqlite;` :916 | `mod.rs` | unchanged |
| inline stub `#[cfg(not(feature))] pub mod sqlite {…}` :923-967 | **`sqlite_stub.rs`** via `#[cfg(not(feature = "codex-sqlite"))] #[path = "sqlite_stub.rs"] pub mod sqlite;` | `pub` |
| *(new)* activity probe | `activity.rs` (`pub mod activity;`) | see §7 |
| `#[cfg(test)] mod tests;` :969 | `mod.rs` | unchanged |

Submodules are declared `pub(crate) mod roots; pub(crate) mod discover;
pub(crate) mod resume; pub mod activity;` and the external API is re-exported from
`mod.rs` with `pub use`.

Verified in a scratch crate: `pub use` of a `pub` item out of a `pub(crate) mod`
is legal and warning-free; `pub(crate) use` of a test-only item is **not** (fires
`unused_imports` under `-D warnings`), which is why the two test-only privates are
reached by explicit path instead.

### `ParsedSession` placement — stays in `discover.rs`

There is **no real cycle**. Rust modules within a crate may reference each other
freely. `sqlite.rs:60`'s `use super::ParsedSession;` resolves through the `mod.rs`
re-export and needs **zero edits**; `discover.rs` calls `super::sqlite::enrich`.
This exact shape was compiled under both feature states — clean. A separate
`types.rs` would buy nothing but a fourth file.

### cfg-stub placement — own file + `#[path]`

Verified compiling with `--features codex-sqlite` and without. This keeps `mod.rs`
at ~60 lines and makes the known `summary()` divergence reviewable in isolation:
the stub returns `Some("codex_sqlite_disabled: compiled without codex-sqlite feature")`
for `Absent` whereas the real one returns `None` (`sqlite.rs:97`). **Preserve the
divergence in the move; do not "fix" it silently.**

### Test-visibility strategy (4 one-line edits total)

| private item | strategy |
|---|---|
| `dirs_home` (`tests.rs:582`) | add `use super::roots::dirs_home;` to `src/integration/codex/tests.rs` — keeps `pub(super)`, no re-export, no dead-import warning |
| `is_rollout_filename` (`tests.rs:1274-1286`) | same, if `use super::*` no longer reaches it |
| `parse_rollout_records` (`sqlite/tests.rs:1319`) | retarget to `crate::integration::codex::discover::parse_rollout_records(...)` |
| `open_readonly` (`sqlite.rs:205`), `assert_readonly` (`:686`), `paths_equivalent` (`:650`) | **no change** — `sqlite.rs` is not touched by this split and `super::` from `sqlite/tests.rs` still resolves |

### Safe move ORDER

Run this check after **every** step, in **both** feature states (the cfg stub is
the fragile part):

```sh
cargo fmt --check \
 && cargo clippy --all-targets --locked -- -D warnings \
 && cargo clippy --all-targets --all-features --locked -- -D warnings \
 && cargo test --all-features --locked
```

0. **Baseline** — run the check on `HEAD` unmodified; record the pass.
1. **`sqlite_stub.rs`** — move `mod.rs:923-967` verbatim into the new file;
   replace with the `#[cfg(not(...))] #[path = …] pub mod sqlite;` declaration.
   Smallest and riskiest-to-get-wrong piece, isolated first. *Check.*
2. **`roots.rs`** — move constants, `effective_root`, `dirs_home`,
   `rollout_roots`, `RolloutRoot`, `RolloutKind`, `is_rollout_filename`; add
   `pub use roots::{ENV_CODEX_HOME, RolloutKind, RolloutRoot, effective_root, rollout_roots};`
   to `mod.rs`; add the two `use super::roots::…` lines to `tests.rs`. *Check.*
3. **`resume.rs`** — move `resume_spec` + `is_default_root`;
   `pub use resume::resume_spec;`. Leaf; depends only on `AGENT` +
   `ENV_CODEX_HOME`. *Check.*
4. **`activity.rs` (signatures only, §7)** — add `pub mod activity;`. Temporarily
   prepend `#![allow(dead_code)]` inside the file (`unimplemented!()` bodies and
   unread fields otherwise trip `-D warnings`); remove it in step 6. *Check.*
5. **`discover.rs`** — move everything remaining except `AGENT`, the doc comment,
   and the `mod` declarations; add
   `pub use discover::{DiscoveredSession, ImportMeta, ParsedSession, build_session, discover, discover_with_filter, discover_with_filter_enriched, parse_rollout_file};`;
   retarget `sqlite/tests.rs:1319`. `mod.rs` is now thin by construction. *Check.*
6. **Implement `activity.rs` + wire `src/app.rs` (§5)**; drop the temporary
   `allow`. *Check*, plus `cargo test --locked` with no features to confirm the
   non-sqlite path.

Steps 1-5 are **pure moves** with no behavior change, so a green `cargo test`
between each is a genuine bisect point. Commit each step separately.

---

## 7. `src/integration/codex/activity.rs` — signature draft

Signatures only, `unimplemented!()` bodies, no implementation. Follows repo
conventions: no `Result` alias, degradable subsystem returns data +
`Vec<Diagnostic>`, `Diagnostic.category` is `&'static str`.

The file is committed alongside this document at
`src/integration/codex/activity.rs`. It is **not** yet declared in `mod.rs`
(per this task's constraint that no other `.rs` file is modified), so it is not
compiled by cargo until move-step 4 above adds `pub mod activity;`.

Reproduced here for review:

```rust
//! Codex activity detection (positive evidence only).
//!
//! A Codex session is reported [`ActivityStatus::Active`] only when a live
//! process whose command name begins with `codex` holds an open file
//! descriptor on the *exact* rollout file backing that session. Absence of
//! such evidence is [`ActivityStatus::Unknown`], never `Inactive`: this probe
//! can fail for reasons that have nothing to do with the session (no `lsof`
//! on `PATH`, unreadable foreign processes, a hardened kernel), and a missing
//! signal must never be rendered as a negative one.
//!
//! ## One probe per discovery run
//!
//! [`probe`] spawns **exactly one** child process for the whole run: `lsof`
//! is asked for the open files of every Codex process at once, and the result
//! is indexed into an [`ActivitySnapshot`]. Discovery then answers each
//! session with a hash lookup, so the number of spawned processes is
//! independent of how many sessions exist. The snapshot is taken before the
//! per-agent discovery threads start and shared immutably.
//!
//! ## Path identity
//!
//! `lsof` reports fully symlink-resolved paths (`/private/tmp/...`) while a
//! [`ParsedSession`][super::ParsedSession] may carry either the canonical or
//! the pre-canonical form, because canonicalization there is best effort. The
//! snapshot therefore indexes each open rollout three ways: by resolved path,
//! by `(device, inode)`, and by file name. A file-name hit alone is never
//! enough — it only selects a candidate that is then confirmed by device and
//! inode, so an ancestor symlink can neither lose nor fabricate evidence.
//!
//! ## Failure isolation
//!
//! `lsof` exits nonzero when any selected process is unreadable, and also
//! when nothing matched at all, even though it still prints every readable
//! row. The exit status is therefore ignored entirely and standard output is
//! parsed on a best-effort, per-record basis: one unreadable process costs
//! that process's evidence and a [`Diagnostic`], never the rest of the batch.
//!
//! ## Consequence for resume
//!
//! An Active session is a risk reason ([`crate::launch::risk_reasons`]), so
//! resuming one always confirms, including under `--no-confirm`. That is the
//! intended guard against two clients writing one rollout file, not a
//! regression: `--no-confirm` has never bypassed risk prompts
//! (`plans/v0.1.0-implementation.md:362`).

use std::{
    collections::HashMap,
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
    time::SystemTime,
};

use crate::session::{ActivityStatus, Diagnostic};

/// External program used to enumerate open file descriptors.
pub(crate) const PROBE_PROGRAM: &str = "lsof";

/// Command-name prefix selecting Codex processes. `lsof -c` matches commands
/// that *begin* with the given characters, so this also selects helper
/// processes such as `codex-code-mode-host`.
pub(crate) const CODEX_COMMAND_PREFIX: &str = "codex";

/// Seconds allowed for each `stat`/`lstat`/`readlink` that `lsof` performs,
/// so a wedged mount cannot stall discovery. `lsof` rejects values below 2.
pub(crate) const PROBE_STAT_TIMEOUT_SECONDS: &str = "2";

/// `lsof` could not be spawned at all. A missing `lsof` is *not* this case:
/// an uninstalled tool degrades silently, mirroring an absent SQLite DB.
pub(crate) const DIAG_PROBE_FAILED: &str = "codex_activity_probe_failed";

/// `lsof` returned usable rows but also reported records it could not read.
pub(crate) const DIAG_PROBE_PARTIAL: &str = "codex_activity_probe_partial";

/// One live process holding an open descriptor on a rollout file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FdEvidence {
    /// PID of the holding process, as reported by `lsof`.
    pub pid: u32,
    /// Command name of the holding process (e.g. `codex`).
    pub command: String,
    /// Path `lsof` reported, already symlink-resolved by the kernel.
    pub path: PathBuf,
    /// Device number of the open file, when a parsable one was reported.
    pub device: Option<u64>,
    /// Inode number of the open file, when a parsable one was reported.
    pub inode: Option<u64>,
}

/// Indexed result of one activity probe, shared across discovery threads.
#[derive(Clone, Debug)]
pub struct ActivitySnapshot {
    /// When the probe was taken; reused verbatim as every `observed_at`.
    observed_at: SystemTime,
    /// Every open rollout descriptor observed, in `lsof` output order.
    entries: Vec<FdEvidence>,
    /// Resolved path to index into `entries`.
    by_path: HashMap<PathBuf, usize>,
    /// `(device, inode)` to index into `entries`.
    by_identity: HashMap<(u64, u64), usize>,
    /// File name to candidate indices, each confirmed by device and inode.
    by_name: HashMap<OsString, Vec<usize>>,
}

impl ActivitySnapshot {
    /// An empty snapshot: every lookup yields Unknown. Used when the probe is
    /// unavailable or failed, and by callers that do not discover Codex.
    pub fn empty() -> Self {
        unimplemented!()
    }

    /// When this snapshot was taken.
    pub fn observed_at(&self) -> SystemTime {
        unimplemented!()
    }

    /// Number of open rollout descriptors observed.
    pub fn len(&self) -> usize {
        unimplemented!()
    }

    /// True when no open rollout descriptor was observed.
    pub fn is_empty(&self) -> bool {
        unimplemented!()
    }

    /// Look up positive evidence for one rollout path.
    ///
    /// Resolution order, cheapest first: exact resolved path, then
    /// `(device, inode)`, then a file-name candidate confirmed by
    /// `(device, inode)`. Only a name candidate can trigger a `stat` of the
    /// caller's path, and at most one, so the syscall cost scales with live
    /// sessions rather than with all discovered sessions.
    pub fn lookup(&self, _rollout_path: &Path) -> Option<&FdEvidence> {
        unimplemented!()
    }

    /// Build the three indexes over already-parsed evidence.
    fn from_entries(_entries: Vec<FdEvidence>, _observed_at: SystemTime) -> Self {
        unimplemented!()
    }
}

/// Determine the [`ActivityStatus`] of a rollout path against an optional
/// snapshot. Active only on a confirmed open descriptor; absence of evidence
/// is Unknown, never Inactive. Mirrors `pi::activity_status` and
/// `omp::activity_status`.
pub fn activity_status(
    _rollout_path: &Path,
    _snapshot: Option<&ActivitySnapshot>,
) -> ActivityStatus {
    unimplemented!()
}

/// Take one activity snapshot for the whole discovery run.
///
/// Spawns at most one child process. Returns an empty snapshot when `lsof`
/// is absent — silently, with no diagnostic, exactly as an absent SQLite DB
/// produces none. Never fails, never panics, never reports Inactive.
pub fn probe() -> (ActivitySnapshot, Vec<Diagnostic>) {
    unimplemented!()
}

/// Probe using an explicit program name and observation time so tests can
/// substitute a stub that replays recorded `lsof` field output.
pub(crate) fn probe_with(
    _program: &OsStr,
    _observed_at: SystemTime,
) -> (ActivitySnapshot, Vec<Diagnostic>) {
    unimplemented!()
}

/// Exact argv passed to [`PROBE_PROGRAM`]:
/// `-n -P -w -S 2 -F0pcfnDi -c codex`. No shell is involved, every argument
/// is a fixed literal, and no session-derived value is ever interpolated.
pub(crate) fn probe_argv() -> Vec<OsString> {
    unimplemented!()
}

/// Enumerate open rollout descriptors from `/proc` without spawning anything.
/// Used only when `lsof` is not installed; unreadable processes are skipped
/// individually and counted, exactly like the `lsof` path.
#[cfg(target_os = "linux")]
pub(crate) fn probe_proc(_observed_at: SystemTime) -> (Vec<FdEvidence>, usize) {
    unimplemented!()
}

/// Parse `lsof -F0` output into rollout evidence.
///
/// Fields are NUL terminated and grouped into newline-terminated sets; a `p`
/// set opens a process context and each following `f` set describes one
/// descriptor. Sets that are truncated, non-UTF-8, or missing a name are
/// skipped individually, and only names accepted by
/// [`is_rollout_filename`][super::roots::is_rollout_filename] are retained.
/// Returns the evidence plus the number of skipped sets, which the caller
/// renders as [`DIAG_PROBE_PARTIAL`].
pub(crate) fn parse_lsof_output(_stdout: &[u8]) -> (Vec<FdEvidence>, usize) {
    unimplemented!()
}

/// Parse an `lsof` `D` field (`0x`-prefixed hexadecimal device number).
/// `None` on any other encoding, in which case the entry is indexed by path
/// and file name only rather than being dropped.
fn parse_device_field(_value: &str) -> Option<u64> {
    unimplemented!()
}

/// Parse an `lsof` `i` field (decimal inode number).
fn parse_inode_field(_value: &str) -> Option<u64> {
    unimplemented!()
}

/// Read `(device, inode)` for a path, to confirm a file-name candidate.
/// `None` when the path cannot be stat'd (e.g. removed mid-scan), which keeps
/// the session Unknown rather than guessing.
fn file_identity(_path: &Path) -> Option<(u64, u64)> {
    unimplemented!()
}
```

---

## 8. Risks

Ordered by severity.

1. **TOP BEHAVIORAL BLAST RADIUS — resuming an Active Codex session will now
   prompt, including under `--no-confirm`.** `risk_reasons`
   (`src/launch.rs:145-147`) pushes `"Session is Active"`; `should_confirm`
   (`src/launch.rs:161-164`) returns true from `risky` alone and deliberately
   ignores `no_confirm`.

   **This is ACCEPTED as correct for this design and is NOT to be relaxed here.**
   Rationale of record: prompting before resuming a genuinely-live Codex session
   is the correct default — two clients on one rollout file is the exact hazard
   the risk gate exists for — and `--no-confirm` not bypassing risk prompts is
   pre-existing documented behavior (`plans/v0.1.0-implementation.md:362`:
   "`--no-confirm` cannot bypass risk prompts"), not a regression introduced by
   this change. It is handled as a **test/doc update** (checklist item 8), with
   **zero** changes to `risk_reasons` / `should_confirm`.

2. **Ordering churn.** `compare_sessions` (`src/session.rs:100-105`) ranks Active
   first, so `--list` / `--json` order and the picker's stream order change for
   anyone with a running Codex.

3. **`--json` becomes nondeterministic for Active sessions.** `print_json`
   renders activity as `format!("{:?}", …)` (`src/app.rs:760`), so an Active
   session emits `"Active { observed_at: SystemTime { tv_sec: …, tv_nsec: … } }"`
   — a wall-clock string. Any golden-file or exact-string JSON assertion must
   match on the `Active` prefix, not equality. (`Unknown` stays the literal
   `"Unknown"`, so existing assertions survive as long as no live Codex runs on CI.)

4. **False positive** from a Codex process holding a rollout FD it is not
   interactively "in" (background compaction, a crashed-but-not-reaped process).
   The claimed semantics are "positive evidence of an open descriptor", which is
   exactly what is measured; acceptable, and stated in the module doc comment.

5. **False negative** when Codex runs under a different `argv[0]` (a wrapper, or a
   rewritten npm shim). Degrades to Unknown — safe direction, but the feature
   silently does nothing.

6. **~43 ms added** to every run that includes the codex agent. The probe is on
   the critical path before the discovery threads start, so it delays first paint
   in the picker. Acceptable, but do not let it grow: never add `-d` / `+D`
   scanning, and never drop the `-c codex` filter (unfiltered output measured
   ~50× larger).

---

## 9. What the implementer does next

Ordered and actionable.

1. **Module moves** — execute §6 steps 0-5, running the four-command check after
   each. Commit each step separately for bisectability. Pure moves, no behavior
   change.
2. **Implement `activity.rs`** per §7: `probe_argv` → `Command::new` → **ignore
   `ExitStatus`** → `parse_lsof_output` → `ActivitySnapshot::from_entries`.
3. **Unit-test `parse_lsof_output`** against a recorded `-F0` byte fixture that
   deliberately contains: a good codex process, a truncated `p` set, an `f` set
   with no `n`, an entry with an unparsable `D`, and a non-rollout name. Assert
   good rows survive and `skipped` is exact.
4. **Unit-test `ActivitySnapshot::lookup`** for the §3 matrix: exact-path hit,
   `/tmp` vs `/private/tmp` skew hit, same-basename-different-inode miss,
   deleted-file miss.
5. **Test `probe_with`** against a fake `lsof` shell script on `PATH` — the
   `executable()` helper at `tests/step9_app.rs:33` already establishes this
   pattern — so the spawn path is covered without depending on live processes.
6. **Wire `src/app.rs`** per §5 (6 insertion points), including the
   `options.agents` gate so `resume -a pi` spawns nothing.
7. **Assert the O(1) budget** with a test that counts invocations of the fake
   `lsof` across a fixture with many sessions: expected exactly **1**.
8. **Required doc/test update for risk #1 — no code change:**
   - a. Rewrite `docs/qa/feature-inventory.csv:80`
     (`session-activity-unknown-default`): it currently asserts "every session's
     activity renders as the Rust debug string for Unknown" and cites
     `codex/mod.rs build_session` as hardcoding Unknown. Narrow the claim to
     "Unknown in the absence of positive evidence, never Inactive", and note that
     Codex now produces `Active` when `lsof` reports an open descriptor on the
     rollout.
   - b. Add a new inventory row for Codex Active detection (probe mechanism, one
     spawn, positive-evidence-only), plus a row recording the accepted
     consequence: **resuming an Active Codex session prompts, including under
     `--no-confirm`**, citing `src/launch.rs:145-147`, `src/launch.rs:161-164`,
     and `plans/v0.1.0-implementation.md:362` as pre-existing documented behavior.
   - c. Update `docs/qa/feature-inventory.csv:81` (`session-sort-order`) to note
     the Active rank is now reachable in practice for Codex.
   - d. Audit `tests/step9_app.rs` snapshot assertions. Today they are safe
     because `PATH` is scrubbed to `$home/bin` (`tests/step9_app.rs:48`) so `lsof`
     is never found — **add an explicit comment at that line recording that the
     scrub is now load-bearing for activity determinism**, so nobody "fixes" it by
     inheriting `PATH`. Verify
     `meaningless_option_combinations_are_usage_errors`
     (`tests/step9_app.rs:171-196`) is untouched: `--list --no-confirm` remains a
     usage error at the CLI layer and never reaches the risk gate.
   - e. Update the `## Activity` section of the codex module doc comment
     (`src/integration/codex/mod.rs:33-38`) from "optional" to "implemented,
     `lsof`-based", and record the resume-prompting consequence there (already
     drafted in §7's module doc).
9. **Final gate:**
   `cargo fmt --check && cargo clippy --all-targets --all-features --locked -- -D warnings && cargo test --all-features --locked`,
   plus the same clippy/test pair with **default** features to exercise the
   `sqlite_stub.rs` path.

### Verification still owed (do not assume)

- **On Linux CI:** confirm `lsof -n -P -w -S 2 -F0pcfnDi -c codex` emits `D`/`i`
  fields in the same shapes (`0x…` hex / decimal). If `D` differs,
  `parse_device_field` returns `None` and the path/name index still carries the
  case — verify that fallback with a real Linux run rather than assuming it.
- **Latency on a machine with many open FDs.** If the probe exceeds ~150 ms,
  revisit `-a -c codex -d ^cwd,^txt,^rtd,^mem` (measured 882 vs 1074 lines on the
  reference machine, same 26 rollouts). Note `-a` is mandatory there, since `-c`
  and `-d` are ORed by default.
- **Manual end-to-end:** with a live `codex` in another terminal, run
  `resume --list` and confirm the live session sorts first; then run `resume` and
  confirm the prompt reads `Risk: Session is Active`.

---

## 10. Deferred decisions (explicitly out of scope)

- **Should an Active session suppress the confirm prompt under `--no-confirm`?**
  Out of scope. `risk_reasons` / `should_confirm` are unchanged by this work.
  Revisit separately once real `Active` values are observable in the wild and
  there is evidence about how often users hit the prompt in scripted/automation
  flows. Any change there is a global risk-gate policy decision affecting all four
  agents, not a Codex detection concern.
- **Pi and OMP evidence producers.** The `None` at `src/app.rs:407` and
  `src/app.rs:531` stays. Separate change, separate blast radius.
- **A `format.rs` module for codex.** Not created; `derive_title` lives beside its
  only caller. Reopen only if codex grows real presentation code.
- **Claude activity detection.** No seam exists
  (`src/integration/claude/mod.rs:513` hardcodes Unknown). Out of scope.
