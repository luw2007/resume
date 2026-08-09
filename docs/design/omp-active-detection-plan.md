# OMP active-detection wiring and module split plan

## Status and scope

**Status:** design only. No `.rs` file is changed by this document. Everything below is an
instruction set for a follow-up implementation series (PR0–PR7).

**In scope**

- Diagnosing why OMP sessions never report `ActivityStatus::Active`, at the line level.
- A concrete wiring plan: process-table acquisition, breadcrumb staging, evidence correlation,
  and the exact call-chain signature changes needed to deliver an `ActivityEvidence` to
  `src/app.rs:531`.
- Splitting `src/integration/omp/mod.rs` (926 lines) into five focused modules behind an
  unchanged public API.
- Splitting `src/integration/omp/tests.rs` (1733 lines, 60 tests) along the same seams.
- Adjudicating the title-priority discrepancy between the code and the task brief.
- Dispositions for dead/misnamed items the split will surface.

**Out of scope**

- Claude and Codex activity detection. Both hardcode `ActivityStatus::Unknown`
  (`src/integration/claude/mod.rs:507-515`, `src/integration/codex/mod.rs:580-588`) and neither
  has an evidence type. They stay `Unknown`.
- Pi activity detection *implementation*. Pi has the identical structural break at
  `src/app.rs:407` and the plan keeps its signatures symmetric so a later PR can wire it, but
  Pi's `SessionControlEvidence` acquisition is not designed here.
- Any change to `ActivityStatus`, `Session`, or the picker/preview rendering path.
- Any change to the resume argv contract.

**Audience:** an engineer with no prior context on this repo. Every claim is cited `file:line`.

---

## TL;DR

`src/app.rs:531` reads:

```rust
omp::activity_status(&parsed, None),
```

That literal `None` is the entire bug. The detection predicate
(`src/integration/omp/mod.rs:833-846`), the evidence type (`848-863`), the correlation gate
(`865-880`) and 2 unit tests covering all 5 branches (`src/integration/omp/tests.rs:1349-1441`)
are all present, correct, and fully covered. Nothing in `src/` ever constructs an
`ActivityEvidence`. The feature is **written but not wired**, not missing.

The fix has three parts:

1. Acquire a process table **once, before agent fan-out**, at `src/app.rs:74-81` — via one
   `ps -Ao pid=,tdev=,etime=,ucomm=` subprocess (measured 40 ms; see
   [Evidence acquisition](#d1--evidence-acquisition-ps-not-ffi-not-sysinfo)).
2. Read OMP terminal breadcrumbs per profile inside `discover_omp` (cheap filesystem reads) —
   **blocked on an unknown**, see [The breadcrumb unknown](#5-the-breadcrumb-unknown).
3. Correlate into an `ActivityEvidenceMap` keyed by canonical transcript path, thread it down
   `run → discover_all/run_interactive → discover_agent → discover_omp`, and replace the `None`
   at `531` with an O(1) map lookup.

Every failure path degrades to `ActivityStatus::Unknown`, which is exactly today's behavior. The
change is strictly additive in risk terms *except* for false positives, which are the top entry
in the [risk register](#10-risk-register).

---

## 3. Current-state diagnosis

### 3.1 Where the detection code lives

All of it is in `src/integration/omp/mod.rs`, verbatim at lines 833–880:

```rust
/// Determine the [`ActivityStatus`] for a parsed session. Active is reported
/// only when a live OMP process, its TTY, and a matching terminal breadcrumb
/// Session path all agree. A stale breadcrumb alone is Unknown; absence of
/// evidence is Unknown, never Inactive.
pub fn activity_status(                                    // 837
    parsed: &ParsedSession,
    evidence: Option<&ActivityEvidence>,
) -> ActivityStatus {
    match evidence {
        Some(evidence) if evidence.matches(parsed) => ActivityStatus::Active {
            observed_at: evidence.observed_at,
        },
        _ => ActivityStatus::Unknown,                      // 844
    }
}

/// Positive-evidence correlation of a live OMP process, its TTY, and a
/// matching terminal breadcrumb. All three must agree for Active.
#[derive(Clone, Debug)]                                    // 851
pub struct ActivityEvidence {                              // 852
    /// Whether a live OMP process was observed.
    pub live_process: bool,                                // 854
    /// The TTY the live process is attached to.
    pub tty: Option<OsString>,                             // 856
    /// Whether a terminal breadcrumb maps this TTY to a Session path.
    pub breadcrumb_alive: bool,                            // 858
    /// Transcript path recorded in the breadcrumb.
    pub breadcrumb_session_path: PathBuf,                  // 860
    /// When the correlation was observed.
    pub observed_at: SystemTime,                           // 862
}

impl ActivityEvidence {
    /// A match requires a live process, a TTY, an alive breadcrumb, and the
    /// breadcrumb Session path resolving to the parsed transcript locator.
    fn matches(&self, parsed: &ParsedSession) -> bool {     // 868 — PRIVATE
        if !self.live_process || self.tty.is_none() || !self.breadcrumb_alive {
            return false;                                  // 869-871
        }
        let self_canon = self.breadcrumb_session_path.canonicalize().ok();
        let parsed_canon = parsed.transcript_path.canonicalize().ok();
        match (self_canon, parsed_canon) {
            (Some(a), Some(b)) => a == b,
            _ => self.breadcrumb_session_path == parsed.transcript_path,  // 876
        }
    }
}
```

Three properties matter for the wiring plan:

- **The struct needs no field change.** `live_process` / `tty` / `breadcrumb_alive` /
  `breadcrumb_session_path` / `observed_at` is exactly the shape a correlator produces.
- **It never returns `Inactive`.** Positive-evidence-only, matching `README.md:146`. Absence of
  a process table, absence of breadcrumbs, a `ps` timeout — all land on `Unknown` at `844`.
- **`matches` is private (`868`)**, callable only from `activity_status`. That is correct and
  the split must preserve it.

The only place an `ActivityStatus` enters an OMP `Session` is
`ParsedSession::into_session` at `src/integration/omp/mod.rs:429` (signature `396-401`).

### 3.2 The full call chain to the break

| Step | Location | What happens |
|---|---|---|
| 1 | `src/main.rs:25` | `std::process::exit(resume::app::run(cli))` |
| 2 | `src/app.rs:60` | `pub fn run(cli: Cli) -> i32`; config load `61`, `effective_options` `67`/`97` (agent default list `["codex","claude","pi","omp"]` at `106`) |
| 3 | `src/app.rs:74` | `let scope = ... Arc::new(scope)` via `build_scope` (`132`) — **the model for where the process snapshot belongs** |
| 4 | `src/app.rs:83` | branch: `--list`/`--json` → `discover_all` (`84`, fn at `155`); else `run_interactive` (`94`, fn at `193`) |
| 5a | `src/app.rs:166-190` | batch fan-out: one `thread::spawn` per agent, joins `186-188`, sort `190` |
| 5b | `src/app.rs:204-232` | interactive fan-out: one thread per agent, streams into `sync_channel(CHANNEL_CAPACITY)` (built `200`), consumed by `picker::run_production_picker` at `235` |
| 6 | `src/app.rs:326` | `fn discover_agent(agent, scope, since_cutoff, cancel) -> AgentDiscovery`; string dispatch `335-341` |
| 7 | `src/app.rs:497` | `fn discover_omp(scope: &Scope) -> AgentDiscovery` |
| 8 | `src/app.rs:522-523` | `for root in roots { omp::discover(&omp::DiscoverConfig::new(root.clone(), scope)) ... }` |
| 9 | `src/app.rs:526-536` | per-parsed-session closure: `resume_spec` `527`, `into_session` `529-532` |
| **10** | **`src/app.rs:531`** | **`omp::activity_status(&parsed, None)` — THE BREAK** |

Both fan-out sites already clone per-thread `scope` (`169` / `207`), `state`, `records`/`map`,
and `cancel`. Adding one more `Arc` clone is a two-line change at each site.

### 3.3 Nothing constructs `ActivityEvidence` outside tests

`ActivityEvidence` is constructed at exactly six places, all in
`src/integration/omp/tests.rs`: lines `1389`, `1402`, `1412`, `1421/1422`, `1428`, `1432/1433`.
Those back the two activity tests:

- `activity_unknown_without_evidence` (`tests.rs:1354`)
- `activity_active_only_with_live_process_tty_and_matching_breadcrumb` (`tests.rs:1373`)

Between them they cover all five branches: full evidence → `Active`; `live_process: false` →
`Unknown`; `breadcrumb_alive: false` → `Unknown`; `tty: None` → `Unknown`; mismatched path →
`Unknown`.

No external test exercises activity. `tests/step9_app.rs:106-115`, `:263-287`, `:33`, `:132` and
`tests/picker_spike.rs:53`, `:586` touch OMP but only for discovery/roots/import-badge.

### 3.4 Sibling integrations confirm the diagnosis

| Agent | Activity code | State |
|---|---|---|
| **omp** | `activity_status` `omp/mod.rs:837`, `ActivityEvidence` `852` | predicate complete; **`None` passed at `app.rs:531`** |
| **pi** | `activity_status` `src/integration/pi.rs:563-601`, `SessionControlEvidence` struct | predicate complete; **identical `None` at `app.rs:407`**; evidence built only at `pi/tests.rs:1037,1048,1059` |
| **claude** | none | `activity: ActivityStatus::Unknown` hardcoded in the `Session` literal, `claude/mod.rs:507-515`. Doc `30-33`: "No authoritative active marker was found." |
| **codex** | none (`rg 'fn activity' src/integration/codex/mod.rs` → no hits) | `activity: ActivityStatus::Unknown` hardcoded, `codex/mod.rs:580-588`. Doc `34-38` describes an intended fd-based design that was never built. |

**Verdict: no working reference implementation of live correlation exists anywhere in this
repo.** What exists is a decision predicate with full unit coverage and zero evidence
acquisition. `README.md:146` is accurate and, if anything, understated.

### 3.5 The consequence is user-visible and already plumbed

- `src/app.rs:693` `status_label` prints `"ACTIVE"` only when
  `matches!(session.activity, ActivityStatus::Active { .. })` (`696`). Dead today.
- `src/launch.rs:143` `risk_reasons` pushes `"Session is Active"` (`146`) when activity is
  `Active`. Dead today.
- `src/launch.rs:161` `should_confirm` treats any risk reason as **mandatory** confirmation;
  the doc comment at `160` states `no_confirm` suppresses only *ordinary* confirmation. Dead
  today. **This is why false positives are the top risk.**

### 3.6 There is no integration trait

`rg '^pub trait|^trait' src` returns only `trait BoolNot` (`omp/mod.rs:233`).
`src/integration/mod.rs:11-14` is four bare `pub mod` declarations; the doc at `3-8` states
integrations "share only the pure helpers under `crate::jsonl` … there is no shared transcript
schema". Each integration is free functions plus a bespoke config struct:

- `omp::discover(&DiscoverConfig<'_>) -> io::Result<DiscoverOutcome>` — `omp/mod.rs:496`
- `pi::discover(&DiscoverConfig<'_>) -> io::Result<DiscoverOutcome>` — `pi.rs:346`
  (Pi's `DiscoverConfig` at `pi.rs:204-227` is byte-identical in shape to OMP's)
- `claude::discover(&ClaudeRoot) -> Result<Discovery, IntegrationError>` — `claude/mod.rs:131`,
  no `Scope` at all; `app.rs:429` filters afterwards
- `codex::discover_with_filter_enriched<F>(...)` — `codex/mod.rs:216-223`

**Implication:** there is no trait to extend. Live evidence must be threaded as an explicit
parameter through the concrete functions. That is a feature, not a limitation — it keeps the
blast radius to the OMP and Pi arms.

---

## 4. Wiring plan

### D1 — Evidence acquisition: `ps`, not FFI, not `sysinfo`

**Decision: shell out to `ps -Ao pid=,tdev=,etime=,ucomm=` exactly once per `resume`
invocation.**

The repo already has exactly one subprocess pattern — `Command::new("git").output()` at
`src/scope.rs:174`, `:207`, `:221`. The process probe follows it verbatim.

#### Measured cost

Benchmarked on the development host (macOS, 1523 processes, 7 runs each, median reported):

| Command | Median | Min | Max | Why |
|---|---:|---:|---:|---|
| **`ps -Ao pid=,tdev=,etime=,ucomm=`** | **40 ms** | 39 ms | 51 ms | **chosen** — numeric `tdev`, no name resolution, no argv read |
| `ps -Ao pid=,tty=,etime=,comm=` | 708 ms | 666 ms | 733 ms | `tty=` forces per-process device-name resolution — **17× slower** |
| `ps auxww` | 709 ms | 687 ms | 715 ms | full argv read, same penalty |
| `lsof -n -P -c omp` | 27 ms | 26 ms | 30 ms | rejected: requires knowing the binary name a priori and is not portable to Linux distros without lsof |

The `tdev=` vs `tty=` gap is the whole decision. `tty=` is convenient (it prints `s000`) but
costs ~670 ms on the picker's critical path. `tdev=` prints the raw device number and costs
40 ms.

> Re-run these on the target platform before locking `PROC_PROBE_BUDGET`. The *ratio* is stable;
> the absolute numbers scale with process count. **This is a hard prerequisite of PR1**, not an
> optional nicety: the budget constant is derived from the median measured here.

#### Exact argv, per-OS and cfg-gated

`tdev` is a BSD column. procps-ng (Debian/Ubuntu, i.e. the `ubuntu-latest` CI runner at
`.github/workflows/ci.yml:31`) does not implement it, and on Linux `tty=` is cheap because there
is no `devname(3)` call to pay for — the kernel already exposes the name. So the argv is selected
at compile time:

```rust
#[cfg(target_os = "macos")]
const PS_ARGS: [&str; 3] = ["-A", "-o", "pid=,tdev=,etime=,ucomm="];
#[cfg(target_os = "linux")]
const PS_ARGS: [&str; 3] = ["-A", "-o", "pid=,tty=,etime=,comm="];
```

- `-A` (BSD style) is accepted by both BSD `ps` and procps-ng.
- The command name is **last** so it can absorb embedded spaces; `pid`, `tdev`/`tty` and `etime`
  are all space-free, so `split_whitespace()` with a `rest.join(" ")` tail is total.
- Deliberately **not** `lstart=` — its output is locale-dependent. `etime`
  (`[[dd-]hh:]mm:ss`) is not.
- On Linux the `tty` field is already a name (`pts/3`, or `?` for none), so the `/dev` scan below
  is a macOS-only step.

See [R9](#the-rest): the Linux path must be verified on the CI runner, not only locally.

#### Resolving `tdev` to a TTY name

`ps -Ao tdev=` prints one of:

- `??` — no controlling terminal. **Discard the row immediately.**
- `16/4` (macOS style, major/minor) or a decimal device number, platform dependent.

Resolution is a single `read_dir("/dev")` pass, done **once**, building
`HashMap<(u32 major, u32 minor), OsString>`:

1. `read_dir("/dev")`, and `/dev/pts` on Linux.
2. For each entry, `metadata().file_type().is_char_device()` (via
   `std::os::unix::fs::FileTypeExt`) — skip anything else.
3. `metadata().rdev()` (via `std::os::unix::fs::MetadataExt`, already used at
   `src/launch.rs:52-70` for `dev()`/`ino()`) gives the packed device number.
4. Store `rdev → file_name()`.

Verified on the dev host: `/dev/ttys000` has major 16, minor 0, packed rdev `268435456`;
`/dev/ttys001` is major 16, minor 1, rdev `268435457`. `ps` reported those same processes as
`16/0` and `16/1`. The mapping is a direct lookup.

The breadcrumb key is then whatever string form the breadcrumb store uses (`ttys000`, `s000`,
`/dev/ttys000`) — normalization is deferred to the [breadcrumb probe](#5-the-breadcrumb-unknown),
which is what tells us the on-disk convention.

#### Rejected: direct libc FFI

`libc v0.2.189` is in `Cargo.lock:742` but **only transitively**
(`skim → skim-tuikit → term → dirs-next → dirs-sys-next`; also `rustix`/`errno` via `tempfile`
and `which`). Promoting it to a direct dependency is the lowest-friction *crate* route, and
`src/picker.rs:460-486` (`tty_size`, with an inline `unsafe extern "C" { fn ioctl(..) }` at
`478-480` and per-OS `TIOCGWINSZ` at `482-485`) proves raw-FFI-without-libc is house style.

Rejected anyway because there is **no portable libc call that enumerates processes**. macOS needs
`sysctl(KERN_PROC_ALL)` plus `struct kinfo_proc` layout knowledge; Linux needs a `/proc` walk.
That is two platform-specific implementations plus an `unsafe` struct-layout dependency, to save
40 ms. Not worth it.

`nix` appears at `Cargo.lock:786` and `:798` but `cargo tree -i nix` is **empty** for the default
feature set — reachable only via `portable-pty` (a dev-dependency) and optional paths. It is
unusable from `src/`.

#### Rejected: `sysinfo`

Pulls a nontrivial dependency subtree into a binary whose entire `[dependencies]` list is
`clap, clap_complete, serde, serde_json, skim, thiserror, toml, unicode-width` plus optional
`rusqlite`. It also must clear `deny.toml`: license allow-list (`12-21`),
`wildcards = "deny"` (`25`), `unknown-registry = "deny"` (`29`). Shelling to `ps` adds **zero**
dependencies and zero `deny.toml` review.

Two further strikes:

- `deny.toml:5` sets `all-features = true`, so the **entire** feature graph of any new direct
  dependency is audited, not just the subset actually compiled. Combined with the MSRV pin
  (`Cargo.toml:5`) this is a standing maintenance liability for a 40 ms saving.
- Its portable `Process` API exposes `name`/`cmd`/`pid`/`start_time` but **no controlling-terminal
  device name**. If that holds — an implementer should confirm it in 30 seconds before citing it —
  `sysinfo` does not even solve the stated problem, because the TTY is the join key of the entire
  correlation.

#### Read-only invariant

`src/snapshot.rs:145` `assert_unchanged` enforces that discovery never mutates agent files.
`ps` is read-only and touches no agent file, so the invariant holds mechanically.

However `src/integration/omp/mod.rs:57` states the module "never invokes OMP during
discovery/preview". Running `ps` is not invoking OMP, but **PR2 must update that doc comment**
to say the module may enumerate the process table read-only, so the wording does not become
misleading.

### D2 — The new `src/proc.rs`

New top-level module, sibling of `src/scope.rs`. Register `pub mod proc;` in `src/lib.rs`
(alphabetically between `picker` and `runtime`, i.e. `src/lib.rs:11`/`12`).

```rust
//! Read-only OS process-table probe. Used to correlate live agent processes
//! with discovered sessions. Never mutates anything; every failure degrades to
//! an empty table, which callers must treat as "no evidence" (Unknown).

use std::{
    ffi::OsString,
    io,
    path::Path,
    time::{Duration, SystemTime},
};

/// Wall-clock budget for the whole probe (subprocess + /dev scan). Exceeding it
/// yields an empty table rather than an error the caller must handle.
///
/// Derivation: 300 ms is ~7x the 40 ms median measured on the development host
/// (macOS, 1523 processes, 7 runs; see "Measured cost" in the design plan).
/// RE-MEASURE on the target platform before trusting this number — the ratio
/// between `tdev=` and `tty=` is stable but the absolute cost scales with
/// process count. A tighter 150 ms (~4x headroom) is equally defensible; the
/// looser bound is chosen deliberately because expiry costs only *evidence*
/// (everything degrades to Unknown), never correctness.
pub const PROC_PROBE_BUDGET: Duration = Duration::from_millis(300);

/// One row of the process table, after TTY-name resolution.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcEntry {
    pub pid: u32,
    /// `ucomm` — the executable base name, not the full argv.
    pub command: OsString,
    /// Resolved controlling-terminal name (e.g. `ttys000`). `None` when the
    /// process has no controlling terminal (`ps` printed `??`) or the device
    /// number did not resolve under /dev.
    pub tty: Option<OsString>,
    /// Process start time, computed as `observed_at - elapsed` where `elapsed`
    /// is parsed from `etime=`. `None` when `etime` is unparseable.
    ///
    /// This is **not** diagnostic-only: it is the discriminator that rejects a
    /// breadcrumb left behind by an earlier process on a recycled TTY. See
    /// [the `breadcrumb_alive` definition](#definition-of-breadcrumb_alive).
    pub started_at: Option<SystemTime>,
}

/// A snapshot of the process table at a single instant.
#[derive(Clone, Debug, Default)]
pub struct ProcessTable {
    entries: Vec<ProcEntry>,
    observed_at: Option<SystemTime>,
}

impl ProcessTable {
    /// An empty table. Correlation against this always produces no evidence.
    pub fn empty() -> Self;

    /// True when the probe did not run or produced nothing.
    pub fn is_empty(&self) -> bool;

    /// When the snapshot was taken. `None` for an empty/failed probe.
    pub fn observed_at(&self) -> Option<SystemTime>;

    /// All entries whose `command` equals `name` exactly (matched on the
    /// `ucomm` base name, so `/usr/local/bin/omp` matches `"omp"`).
    pub fn by_command(&self, name: &str) -> impl Iterator<Item = &ProcEntry>;

    /// All distinct TTY names held by processes named `name`.
    pub fn ttys_for_command(&self, name: &str) -> Vec<OsString>;

    /// **O(1)**: the live process named `command` occupying `tty`, if any.
    /// Backed by a private `HashMap<OsString, usize>` TTY index built once at
    /// construction. This is the accessor the correlation loop uses, because
    /// it must recover the matching process's `started_at` per breadcrumb.
    pub fn live_on_tty(&self, command: &str, tty: &OsStr) -> Option<&ProcEntry>;

    /// Test/injection constructor: builds a table from literal rows with no
    /// subprocess. Unit tests never shell out.
    pub fn from_entries(entries: Vec<ProcEntry>, observed_at: SystemTime) -> Self;
}

/// Strip a leading `/dev/` and reject `?`, `??`, and empty. Breadcrumbs may
/// store `/dev/ttys009` while `ps` reports `ttys009`; every TTY string on
/// either side of the correlation passes through here first.
pub fn normalize_tty(raw: &OsStr) -> Option<OsString>;

/// Take a process-table snapshot. Returns `Ok(ProcessTable::empty())` — never
/// an error — when `ps` is missing, exits non-zero, produces unparseable
/// output, or exceeds [`PROC_PROBE_BUDGET`]. `Err` is reserved for conditions
/// the caller could act on; today there are none, and callers should treat any
/// `Err` as `empty()` too.
///
/// Honours the `RESUME_DISABLE_PROC_PROBE` kill switch: when that variable is
/// set to any value the subprocess is never spawned and `empty()` is returned
/// with no diagnostic. This is both the operator escape hatch and the
/// hermeticity lever for the binary-driving integration tests.
pub fn snapshot() -> io::Result<ProcessTable>;

/// Test seam: parse a captured `ps -Ao pid=,tdev=,etime=,ucomm=` payload
/// against a caller-supplied device map. Lets unit tests cover parsing with no
/// subprocess and no real /dev.
pub fn parse_ps_output(
    raw: &str,
    devices: &DeviceMap,
    observed_at: SystemTime,
) -> ProcessTable;

/// Map of char-device (major, minor) to terminal name, built from /dev.
#[derive(Clone, Debug, Default)]
pub struct DeviceMap { /* HashMap<(u32, u32), OsString> */ }

impl DeviceMap {
    /// Scan `/dev` (and `/dev/pts` where present) for character devices.
    /// A partial or failed scan yields a partial or empty map, never an error.
    pub fn scan(dev_root: &Path) -> Self;
    pub fn lookup(&self, major: u32, minor: u32) -> Option<&OsString>;
}
```

Design notes:

- `snapshot()` is infallible in practice. Every degradation path returns `empty()`. This is what
  makes "falls back to today's behavior" mechanically true rather than aspirational.
- `parse_ps_output` + `DeviceMap::scan(dev_root)` are the test seams: parsing and device
  resolution are both unit-testable with fixtures and a `tempfile` dir, no subprocess required.
- `#[cfg(unix)]` gates the whole module's real implementation. On non-Unix, `snapshot()` returns
  `empty()`. (The fake-binary resume tests at `omp/tests.rs:1618`, `:1652` are already
  `#[cfg(unix)]`; this matches.)
- `ProcEntry` derives `PartialEq, Eq` for test assertions. Note `ActivityEvidence` derives only
  `Clone, Debug` (`omp/mod.rs:851`); see [R6](#10-risk-register).

### D3 — Breadcrumb staging: `BreadcrumbSource` + `NullBreadcrumbs`

The breadcrumb store location is **not known** (see [section 5](#5-the-breadcrumb-unknown)).
Rather than block the whole wiring on that unknown, PR2/PR3 land the process half behind a
trait-shaped seam with a null default, and PR5 fills in the real reader once the probe answers
the question.

In `src/integration/omp/activity.rs`:

```rust
/// Maps a live TTY name to the transcript path OMP last recorded for it.
///
/// Implementations are read-only and must never fail loudly: an unreadable or
/// absent store yields `None` for every lookup, which degrades to Unknown.
/// One terminal breadcrumb: a TTY device mapped to a session transcript.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Breadcrumb {
    /// Bare device name, `/dev/` stripped: `ttys009` (macOS) / `pts/3` (Linux).
    pub tty: OsString,
    /// Transcript path recorded in the breadcrumb.
    pub session_path: PathBuf,
    /// When the breadcrumb was written, if the record carries a timestamp;
    /// otherwise the source substitutes the file mtime. `None` only when the
    /// source can supply neither.
    pub recorded_at: Option<SystemTime>,
}

pub trait BreadcrumbSource {
    /// Every breadcrumb the source can see. Read-only and never fails loudly:
    /// an unreadable or absent store yields an empty `Vec`, which degrades to
    /// Unknown for every session.
    fn breadcrumbs(&self) -> Vec<Breadcrumb>;
}

/// The no-op source used until the on-disk breadcrumb format is pinned down.
/// Always returns an empty `Vec`, so `ActivityEvidenceMap` is always empty and
/// every session reports Unknown — byte-identical to today's behavior.
#[derive(Clone, Copy, Debug, Default)]
pub struct NullBreadcrumbs;

impl BreadcrumbSource for NullBreadcrumbs {
    fn breadcrumbs(&self) -> Vec<Breadcrumb> { Vec::new() }
}
```

Correlation takes `&dyn BreadcrumbSource` rather than a generic parameter: there is exactly one
production implementation and one test fake, so monomorphization buys nothing and a trait object
keeps `correlate_live_with` a single non-generic symbol.

**Why a `Vec<Breadcrumb>` and not a `session_for_tty(&OsStr)` lookup.** A per-TTY lookup cannot
carry `recorded_at`, and `recorded_at` is what distinguishes a live breadcrumb from a stale one
left by an earlier process on the same recycled TTY. Returning whole records keeps the staleness
rule (below) expressible; a lookup-shaped trait would force the caller to trust the TTY match
alone, which is exactly the false positive [R1](#10-risk-register) is about.

#### Definition of `breadcrumb_alive`

`ActivityEvidence::breadcrumb_alive` (`omp/mod.rs:858`) is defined as:

> **A breadcrumb is *alive* when (a) a breadcrumb exists for TTY *T*, (b) a live process named
> `omp` currently occupies *T*, (c) the breadcrumb names a transcript path that still exists on
> disk, and (d) `breadcrumb.recorded_at >= that process's started_at`.**

Rationale, from `docs/research/session-formats.md:127` and `omp/mod.rs:51-54`: breadcrumbs "can
be stale and do not contain PID". Staleness is therefore *not* detectable from the breadcrumb
alone — it is detectable only by intersecting it with the process table. So:

- The **process table** supplies `live_process`, `tty`, and `started_at`.
- The **breadcrumb source** supplies `breadcrumb_session_path` and `recorded_at`.
- `breadcrumb_alive` is the **conjunction** of (a)–(d).

**Why (d) is not optional.** Conditions (a)–(c) alone are not enough. TTY device names are
recycled: `ttys003` is handed out again to the next terminal. A breadcrumb written by yesterday's
`omp` on `ttys003`, pointing at yesterday's transcript, survives on disk; if a *different* `omp`
is running on `ttys003` today, (a)–(c) all hold and the **wrong** session is reported `Active`.
That is a false positive, and per [R1](#10-risk-register) a false positive is the only outcome
that makes this feature worse than not shipping it. Comparing `recorded_at` against the current
process's `started_at` is the exact predicate that makes *"a stale marker alone is Unknown"*
(`omp/mod.rs:53-54`, `README.md:153`) mechanically true rather than aspirational. This is why
`etime=` is in the `ps` argv from PR1: it is free (measured) and it is the discriminator.

**Fallback ladder when `started_at` is `None`** (unparseable `etime`): fall back to a fixed
freshness window instead of dropping the check entirely.

```rust
/// Fallback staleness window used only when a process's `started_at` could not
/// be derived from `etime=`. Strictly weaker than the start-time comparison.
pub const BREADCRUMB_FRESHNESS: Duration = Duration::from_secs(12 * 60 * 60);
```

i.e. accept the breadcrumb iff `now - recorded_at <= BREADCRUMB_FRESHNESS`. Emit
`Diagnostic { category: "omp_breadcrumb_start_time_unavailable" }` when this path is taken, so it
is visible under `--verbose` rather than silently degrading the guarantee. If `recorded_at` is
also `None`, the breadcrumb is rejected — no evidence, `Unknown`.

**Deliberately NOT used as a liveness signal:**

- A TTL applied to a breadcrumb *whose process start time is known*. Long-idle sessions are still
  active; a bare TTL would produce false negatives on a genuinely live session. The 12 h window
  above is a degraded fallback for the unparseable-`etime` case only, not the primary rule.

### D4 — Snapshot placement: `src/app.rs:74-81`, once, before fan-out

Insert immediately after the `Arc<Scope>` construction at `src/app.rs:74-81` and before the
`--list`/`--json` branch at `83`:

```rust
// src/app.rs, after the scope block ending at :81
let discovery_ctx = Arc::new(DiscoveryContext::probe(&options));
```

with, alongside `EffectiveOptions`:

```rust
/// Process-level evidence shared by every agent discovery worker. Built once
/// per invocation, before agent fan-out, and cloned per thread.
///
/// Every field degrades to "no evidence": an empty `ProcessTable` makes every
/// correlation produce nothing, and every session reports Unknown — which is
/// the pre-wiring behavior.
pub struct DiscoveryContext {
    /// Snapshot of the OS process table, or an empty table if probing failed,
    /// timed out, or was disabled.
    pub procs: crate::proc::ProcessTable,
    /// Diagnostics accumulated while probing (`proc_probe_failed`,
    /// `proc_probe_timeout`).
    pub diagnostics: Vec<Diagnostic>,
}

impl DiscoveryContext {
    /// Probe the process table subject to `PROC_PROBE_BUDGET`. Never fails.
    pub fn probe(options: &EffectiveOptions) -> Self { /* ... */ }

    /// A context with no evidence. Used by tests and by the disabled path.
    pub fn none() -> Self { /* ... */ }
}
```

`probe` skips the subprocess entirely when neither `"omp"` nor `"pi"` is in `options.agents`
(the default list is at `src/app.rs:106`), so `resume --agent claude` pays nothing.

`diagnostics` is drained into `DiscoveryState.errors` inside `discover_all` (`app.rs:159-161`)
and `run_interactive` (`app.rs:196-198`), where the scope-warning diagnostic is already pushed.
Category names: `"proc_probe_failed"` and `"proc_probe_timeout"` — agent-neutral, because
`src/proc.rs` is agent-neutral by design and pi will reuse the same probe. Follows `count_errors`
(`app.rs:606`) and the existing categories at `app.rs:500`/`538`.

**Why `74-81` and not the alternatives**

- One construction site covering both the batch and interactive paths, mirroring exactly how
  `Arc<Scope>` is built once at `74` and cloned per thread at `169`/`207`.
- The alternative — building it at the top of `discover_all` (`156-164`) *and* `run_interactive`
  (`194-202`) — works and mirrors how `DiscoveryState`/`CancelToken` are built, but doubles the
  construction sites for no gain.
- Anything lower is a correctness bug; see [why this is not O(N)](#why-this-is-not-on-subprocesses).

### D5 — Exact changed signatures

| File:line | Before | After |
|---|---|---|
| `src/app.rs:84` | `discover_all(&options, scope)` | `discover_all(&options, scope, discovery_ctx)` |
| `src/app.rs:94` | `run_interactive(&options, scope)` | `run_interactive(&options, scope, discovery_ctx)` |
| `src/app.rs:155-158` | `fn discover_all(options: &EffectiveOptions, scope: Arc<Scope>)` | `fn discover_all(options: &EffectiveOptions, scope: Arc<Scope>, ctx: Arc<DiscoveryContext>)` |
| `src/app.rs:193` | `fn run_interactive(options: &EffectiveOptions, scope: Arc<Scope>) -> i32` | `fn run_interactive(options: &EffectiveOptions, scope: Arc<Scope>, ctx: Arc<DiscoveryContext>) -> i32` |
| `src/app.rs:326-331` | `fn discover_agent(agent: &str, scope: &Scope, since_cutoff: Option<SystemTime>, cancel: &CancelToken) -> AgentDiscovery` | `fn discover_agent(agent: &str, scope: &Scope, ctx: &DiscoveryContext, since_cutoff: Option<SystemTime>, cancel: &CancelToken) -> AgentDiscovery` |
| `src/app.rs:336` | `"pi" => discover_pi(scope)` | `"pi" => discover_pi(scope, ctx)` |
| `src/app.rs:337` | `"claude" => discover_claude(scope)` | unchanged |
| `src/app.rs:338` | `"codex" => discover_codex(scope)` | unchanged |
| `src/app.rs:339` | `"omp" => discover_omp(scope)` | `"omp" => discover_omp(scope, ctx)` |
| `src/app.rs:383` | `fn discover_pi(scope: &Scope) -> AgentDiscovery` | `fn discover_pi(scope: &Scope, ctx: &DiscoveryContext) -> AgentDiscovery` (parameter accepted, unused until the Pi PR; `let _ = ctx;`) |
| `src/app.rs:497` | `fn discover_omp(scope: &Scope) -> AgentDiscovery` | `fn discover_omp(scope: &Scope, ctx: &DiscoveryContext) -> AgentDiscovery` |

`claude` and `codex` keep their signatures. They have no evidence type and no design; adding an
unused parameter to them would be speculative.

`omp::DiscoverConfig` (`omp/mod.rs:315-324`) is **not** modified. Four reasons, in order of
weight:

1. `activity_status` is *already* called outside `discover` — at `app.rs:531`. `discover`
   (`mod.rs:489-545`) never touches `ActivityStatus` at all, so adding a live-evidence field
   would put process data into a type that does not consume it.
2. `DiscoverConfig` is constructed by struct literal in tests (imported at `omp/tests.rs:38`);
   a fourth field forces either a churn diff across every one of those or a `new_with_live`
   constructor that bit-rots.
3. `pi::DiscoverConfig` (`pi.rs:204-227`) is byte-identical in shape. Keeping both untouched
   preserves the symmetry that makes the later Pi work a near-copy of the OMP work.
4. The cost of being wrong is one field. The smaller diff wins by default.

Evidence is therefore threaded only through the app-layer map closure.

### D6 — Building the evidence map, and what replaces the `None`

Inside `discover_omp`, after the `roots` vector is assembled (`app.rs:503-519`) and before the
`for root in roots` loop at `522`:

```rust
// src/app.rs, inserted before :522
let live = omp::correlate_live(&ctx.procs, &roots);
```

with, in `src/integration/omp/activity.rs`:

```rust
/// O(1)-lookup map from canonical transcript path to the evidence that a live
/// OMP process currently owns it.
#[derive(Clone, Debug, Default)]
pub struct ActivityEvidenceMap {
    /* HashMap<PathBuf, ActivityEvidence>, keyed by canonicalized path with a
       lexical-path secondary map for uncanonicalizable entries, mirroring the
       fallback at mod.rs:876 */
}

impl ActivityEvidenceMap {
    /// Evidence for `transcript_path`, or `None`. Canonicalizes once per
    /// lookup and falls back to a lexical match, mirroring
    /// `ActivityEvidence::matches` (mod.rs:872-877).
    pub fn for_transcript(&self, transcript_path: &Path) -> Option<&ActivityEvidence>;

    /// True when no live evidence was found. Callers can skip work.
    pub fn is_empty(&self) -> bool;
}

/// Correlate a process snapshot with OMP breadcrumbs into an evidence map.
/// Uses the null breadcrumb source until the on-disk format is pinned.
pub fn correlate_live(
    procs: &crate::proc::ProcessTable,
    roots: &[EffectiveRoots],
) -> ActivityEvidenceMap;

/// Correlation with an explicit breadcrumb source. This is the unit-testable
/// entry point; `correlate_live` calls it with `NullBreadcrumbs` today and with
/// the real reader after PR5. Pure: no subprocess, no clock read — `now` is
/// passed in so tests are deterministic.
pub fn correlate_live_with(
    procs: &crate::proc::ProcessTable,
    breadcrumbs: &dyn BreadcrumbSource,
    now: SystemTime,
) -> ActivityEvidenceMap;
```

Note `correlate_live_with` deliberately takes no `&EffectiveRoots`: the *caller* builds the
`BreadcrumbSource` from roots, which keeps `activity.rs` independent of `roots.rs` and keeps the
correlation itself a pure function of (process table, breadcrumbs, clock).

Correlation algorithm (`correlate_live_with`), cost `O(B)` where `B` = breadcrumbs, since every
process lookup is an O(1) hash probe:

1. For each `Breadcrumb` from `breadcrumbs.breadcrumbs()`, normalize its `tty` via
   `proc::normalize_tty`. `None` → skip.
2. `procs.live_on_tty(AGENT, &tty)` — `AGENT` is `"omp"` (`omp/mod.rs:81`). `None` → skip: no
   live `omp` on that TTY, so the breadcrumb is stale by construction. **O(1).**
3. **Staleness gate.** If the matched entry has `started_at: Some(t)`, require
   `breadcrumb.recorded_at >= t`; otherwise require
   `now - recorded_at <= BREADCRUMB_FRESHNESS` and emit
   `omp_breadcrumb_start_time_unavailable`. `recorded_at: None` → skip. See
   [the `breadcrumb_alive` definition](#definition-of-breadcrumb_alive).
4. Confirm the path exists (`Path::exists`). Absent → skip (the "still exists on disk" half of
   `breadcrumb_alive`).
5. Emit `ActivityEvidence { live_process: true, tty: Some(tty), breadcrumb_alive: true,
   breadcrumb_session_path: path, observed_at: now }`, keyed by canonicalized `path`.

Note the emitted evidence always has all three gates true — steps 1–4 *are* the gate. That is
deliberate: `ActivityEvidence::matches` (`omp/mod.rs:868-880`) re-checks them, and the
belt-and-braces re-check costs nothing and keeps the unit tests at `tests.rs:1373` meaningful.
The map is a **filter**, never a **decider**: a key miss yields `None` → `Unknown`, and a key hit
is still re-validated by `matches`.

Then, `src/app.rs:531`:

```rust
// before
omp::activity_status(&parsed, None),
// after
omp::activity_status(&parsed, live.for_transcript(&parsed.transcript_path)),
```

`live` is created once outside the `for root in roots` loop at `522` and borrowed by the closure
at `526-536`. The lookup is a hash probe. **This is the entire user-visible change.**

The symmetric change at `src/app.rs:407` for Pi is deferred to its own PR; the `discover_pi`
signature is widened in PR3 so that PR is a one-line diff.

### D7 — Arc clone at both fan-out sites

`src/app.rs:166-178` (batch), alongside the existing `scope`/`state`/`records`/`cancel` clones:

```rust
let ctx = ctx.clone();
// ...
let result = discover_agent(&agent, &scope, &ctx, since_cutoff, &cancel);
```

`src/app.rs:204-217` (interactive), identically, alongside
`scope`/`state`/`map`/`next_key`/`cancel`/`tx`:

```rust
let ctx = ctx.clone();
// ...
let result = discover_agent(&agent, &scope, &ctx, since_cutoff, &cancel);
```

Two lines added at each site. `DiscoveryContext` is `Send + Sync` (it holds a `Vec<ProcEntry>`
of owned `OsString`/`u32` and a `Vec<Diagnostic>`), so `Arc` alone suffices — no `Mutex`.

### Why this is not O(N) subprocesses

This is the single most important implementation constraint. **The trap is at
`src/app.rs:526-536`** — the closure inside
`records.extend(outcome.parsed.into_iter().map(|parsed| { ... }))`.

Line `531` is where the `None` sits, so computing evidence *there* is the natural, obvious, and
wrong move. It runs one process scan **per session**. And it is worse than O(N): the closure is
nested inside the `for root in roots` profile loop at `522`, so the cost is
`sessions × profiles`. At 40 ms per `ps`, 200 sessions across 3 profiles is 24 seconds.

The identical trap exists at `src/app.rs:400-413` for Pi.

**The secondary, subtler trap:** hoisting to the top of `discover_omp` (`app.rs:498`) fixes the
per-session multiplication but still yields **one snapshot per agent thread**. Both `omp` and
`pi` want process data, and `app.rs:166`/`204` spawn one thread per agent, so that is up to
2 concurrent `ps` invocations (4 if `claude`/`codex` were ever wired) — all racing on the
picker's critical path, all producing near-identical data.

**Correct placement: exactly once, before fan-out, at `src/app.rs:74-81`.** One subprocess per
`resume` invocation, no matter how many agents, profiles, or sessions.

Cost accounting after correct placement:

| Work | Frequency | Cost |
|---|---|---|
| `ps -Ao pid=,tdev=,etime=,ucomm=` | 1× per invocation | ~40 ms |
| `/dev` char-device scan | 1× per invocation | ~1 ms |
| `correlate_live` | 1× per agent that needs it (≤2) | O(P + T), microseconds |
| `for_transcript` lookup | 1× per session | O(1) hash probe |
| Breadcrumb reads | 1× per profile, filesystem only, **no subprocess** | negligible |

Breadcrumb reads are per-profile filesystem reads and can stay inside `discover_omp`. Only the
**process table** must be hoisted above the `for agent` loops.

### PROC_PROBE_BUDGET and degradation paths

**`PROC_PROBE_BUDGET = Duration::from_millis(300)`.**

**Derivation, stated explicitly so it can be re-derived rather than cargo-culted:** the measured
median of `ps -Ao pid=,tdev=,etime=,ucomm=` on the development host (macOS, 1523 processes, 7
runs) is **40 ms**, with a worst observed run of 51 ms. 300 ms is **~7× that median**, which
absorbs a loaded machine, a process count several times larger, and a cold page cache, while
still capping the worst case well under a second. A tighter bound of ~4× (150 ms) is equally
defensible and would still clear the measured worst case by 3×; 300 ms is chosen because
**expiry costs only evidence, never correctness** — a blown budget degrades to `Unknown`, i.e.
today's output, so the asymmetry favours the looser bound.

> **PR1 must re-measure on the target platform and, if the median differs materially from 40 ms,
> re-derive this constant as ~7× the newly measured median and update both the constant's doc
> comment and this section.** Do not copy the number forward without the measurement.

The budget matters because the probe sits **outside** the cancellation machinery: the
`CancelToken` checks at `app.rs:333` and `app.rs:219` are per-agent-thread, and `JOIN_BUDGET`
(`src/runtime.rs:23`, 250 ms) governs worker joins, not a subprocess spawned before fan-out.
Note also that the probe runs **before any thread exists**, so no `CancelToken` could cover it
even in principle. The probe therefore needs its own timeout. `std::process::Child` has no timed
wait, so `snapshot()` spawns the collect-and-parse on a helper thread and does
`rx.recv_timeout(PROC_PROBE_BUDGET)`; on expiry it returns `ProcessTable::empty()` and leaves the
helper detached to reap the child — the same detach-rather-than-kill philosophy documented at
`src/runtime.rs:7-9`.

The constant itself lives in `src/runtime.rs` beside `JOIN_BUDGET` (`:23`), so all wall-clock
budgets stay in one place, and is re-exported/referenced from `src/proc.rs`.

Every degradation path:

| Failure | Detection | Result | User-visible effect |
|---|---|---|---|
| `ps` binary absent (`ErrorKind::NotFound`), e.g. a minimal musl container | `Command::spawn` error | `ProcessTable::empty()` | all Unknown = today; diagnostic `proc_probe_failed` |
| `ps` exits non-zero | `ExitStatus::success() == false` | `empty()` | same |
| `ps` exceeds `PROC_PROBE_BUDGET` | `recv_timeout` on the helper thread | `empty()` + diagnostic `proc_probe_timeout`; child left detached to be reaped | same |
| `ps` output unparseable | per-row parse failure | that **row** dropped and counted; other rows kept | partial evidence; strictly a subset of Active |
| `tdev` is `??` (macOS) / `tty` is `?` (Linux) | literal match | row dropped | process has no TTY; cannot be Active by definition (`mod.rs:870`) |
| `tdev` does not resolve under `/dev` | `DeviceMap::lookup` → `None` | `tty: None` | `matches` returns false at `mod.rs:870` → Unknown |
| `/dev` unreadable | `read_dir` error | empty `DeviceMap` → every `tty: None` | all Unknown = today |
| breadcrumb store absent/unreadable | `breadcrumbs()` → empty `Vec` | no evidence emitted | all Unknown = today |
| breadcrumb names a nonexistent path | `Path::exists` false | evidence not emitted | all Unknown = today |
| breadcrumb `recorded_at` < process `started_at` | staleness gate, step 3 | evidence not emitted | all Unknown = today — **the recycled-TTY false positive is blocked here** |
| breadcrumb `recorded_at` is `None` and `etime` unparseable | both ladder rungs fail | evidence not emitted + diagnostic `omp_breadcrumb_start_time_unavailable` | all Unknown = today |
| breadcrumb path ≠ transcript path | `matches` at `mod.rs:872-877` | `false` | Unknown |
| non-Unix platform | `#[cfg(unix)]` | `empty()`, **no diagnostic** (not an error condition) | all Unknown = today |
| `RESUME_DISABLE_PROC_PROBE` set to any value | env check before spawn | `empty()`, no diagnostic | all Unknown = today — operator kill switch + test hermeticity |
| no `omp`/`pi` in `options.agents` | checked in `probe` | probe skipped entirely | no cost |

**Every single path lands on `ActivityStatus::Unknown`, which is byte-for-byte today's output.**
There is no path that produces a worse result than the status quo, and no path that produces an
error the user must act on. That is the safety argument for the whole design.

---

## 5. The breadcrumb unknown

**State this plainly: the on-disk breadcrumb location and format are not pinned anywhere in this
repository.**

- `ActivityEvidence::breadcrumb_session_path` (`src/integration/omp/mod.rs:860`) is *consumed*
  by `matches` (`874`) but never *produced* by any code in `src/`.
- No fixture in `src/integration/omp/tests.rs` or `tests/` writes a breadcrumb. The six
  construction sites (`tests.rs:1389, 1402, 1412, 1421/1422, 1428, 1432/1433`) all pass a path
  the test itself invented.
- `src/integration/omp/mod.rs:51-54` describes breadcrumb *semantics* ("map TTY names to cwd/
  session path", "can be stale", "contain no PID") but gives no path.
- `docs/research/session-formats.md:127` asserts breadcrumbs exist —
  *"Terminal breadcrumbs map TTY names to cwd/session path but can be stale and do not contain
  PID. Report Active only after correlating a live OMP process, its TTY, and matching breadcrumb
  Session path."* — **without giving a path, filename, or format.**
  `session-formats.md:131` lists "live/stale breadcrumbs" as a required fixture that was never
  built.

**Do not invent a path.** A guessed path silently produces zero evidence, which is
indistinguishable from correct behavior and will not fail any test — the worst possible failure
mode. Run the probe first.

### Task: `omp-breadcrumb-probe`

Read-only investigation against a locally installed OMP (`17.2.10` per
`docs/research/session-formats.md`). Deliverable: an amendment to
`docs/research/session-formats.md` and a filled-in `OmpBreadcrumbs` implementation of
`BreadcrumbSource`. **This is PR0 and it gates PR5.**

- [ ] **1. Establish a baseline.** Snapshot the candidate roots before starting an OMP session:
      `$HOME/.omp` (`DEFAULT_BASE_RELATIVE`, `mod.rs:95`), `$XDG_DATA_HOME`
      (`ENV_XDG_DATA_HOME`, `mod.rs:104`), `$XDG_RUNTIME_DIR`, `$TMPDIR`, `/tmp`, `/var/folders`
      (macOS), and `$HOME/.cache`. Use `find <root> -newermt` bracketing or a recursive
      checksum. `src/snapshot.rs` already provides `DirSnapshot`/`diff_snapshots` for this.
- [ ] **2. Start an interactive OMP session on a known TTY.** Record the TTY name from `tty(1)`
      and the PID. Send one message so a transcript is definitely created and flushed.
- [ ] **3. Diff the roots.** Identify every file created or modified since step 1. Filter out
      the transcript JSONL itself (it is under the agent root, `mod.rs:281-305`) and the usual
      log/cache churn.
- [ ] **4. Confirm the correlation.** For each candidate file, verify it contains **both** the
      TTY name from step 2 **and** a path or ID resolving to the transcript from step 2. A file
      containing only one of the two is not the breadcrumb.
- [ ] **5. Record the exact format.** Capture: absolute path template (with which env vars
      parameterize it); one-file-per-TTY vs single-index-file; serialization (JSON / JSONL /
      TOML / bare text); the exact key names for TTY and session path; whether the session path
      is absolute, relative, or an ID needing resolution against `session_root`
      (`EffectiveRoots::session_root`, `mod.rs:144`); and **the exact string form of the TTY key**
      (`ttys000` vs `s000` vs `/dev/ttys000`) — this determines the normalization in
      `DeviceMap`.
- [ ] **6. Determine staleness behavior.** Exit the OMP session from step 2 and re-diff. Does
      the breadcrumb get deleted, truncated, or left in place? If left in place, it confirms the
      [`breadcrumb_alive` definition](#definition-of-breadcrumb_alive): staleness is only
      detectable by intersecting with the process table, never from the file alone.
- [ ] **7. Test the multi-profile case.** Repeat steps 2–6 with `--profile <name>`. Determine
      whether breadcrumbs are per-profile (under `<config_root>/profiles/<name>/…`,
      cf. `mod.rs:299-303`) or global. **If global, breadcrumb reads must be hoisted out of the
      `for root in roots` loop at `app.rs:522`, exactly like the process table** — otherwise the
      same file is re-read once per profile.

**Until PR0 completes, `NullBreadcrumbs` is the shipped implementation and every session
correctly reports `Unknown`.** PR2–PR4 are mergeable and useful without it: they land the
process probe, the plumbing, and the tests.

---

## 6. Module split plan

`src/integration/omp/mod.rs` is 926 lines mixing root resolution, filesystem discovery, JSONL
parsing, resume-argv construction, and activity correlation. Split into five siblings under
`src/integration/omp/`, behind an unchanged public API.

### Assignment table

Every item in `mod.rs` is assigned to **exactly one** destination.

| Source lines | Item | → File | Post-split visibility |
|---|---|---|---|
| 1–77 | module docs, imports | `mod.rs` (docs) / per-file (imports) | — |
| 81 | `AGENT` | `mod.rs` | `pub` (re-exported; used by `format`, `resume`, `activity`) |
| 84 | `ENV_CONFIG_DIR` | `roots.rs` | `pub` (re-exported; used by `resume::resume_env`) |
| 86 | `ENV_AGENT_DIR` | `roots.rs` | `pub` (re-exported) |
| 88 | `ENV_SESSION_DIR` | `roots.rs` | `pub` (re-exported), **name unchanged by the split** — renamed to `SESSION_DIR_FLAG` in the cleanup PR, see [dead code D3](#9-dead-code-dispositions) |
| 90 | `ENV_PI_PROFILE` | `roots.rs` | `pub` (re-exported) |
| 92 | `ENV_OMP_PROFILE` | `roots.rs` | `pub` (re-exported) |
| 95 | `DEFAULT_BASE_RELATIVE` | `roots.rs` | `pub` (re-exported) |
| 97 | `AGENT_DIR_NAME` | `roots.rs` | `pub` (re-exported) |
| 99 | `PROFILES_DIR_NAME` | `roots.rs` | **`pub` + re-exported — used externally at `src/app.rs:503`** |
| 101 | `PROFILE_AGENT_DIR_NAME` | `roots.rs` | `pub` (re-exported) |
| 104 | `ENV_XDG_DATA_HOME` | `roots.rs` | `pub` (re-exported) |
| 107 | `DISCOVERY_SCAN_RECORDS` | `discover.rs` | private to `discover` |
| 109–117 | `enum ProfileSelection` | `roots.rs` | `pub` (re-exported; used by `resume:437`, `format:421`) |
| 122–127 | `ProfileSelection::as_profile_field` | `roots.rs` | `pub` |
| 129–152 | `struct EffectiveRoots` | `roots.rs` | `pub` (re-exported; consumed by `discover:490,517`, `resume`, `format:420-421`) |
| 154–174 | `struct ResolutionInputs` | `roots.rs` | `pub` (re-exported) |
| 181–192 | `ResolutionInputs::from_env` | `roots.rs` | `pub` |
| 195–198 | `with_profile_flag` | `roots.rs` | `pub` (moved untouched; **deleted** in the cleanup PR — see [dead code D1](#9-dead-code-dispositions)) |
| 201–204 | `with_session_dir_flag` | `roots.rs` | `pub` (moved untouched; **deleted** in the cleanup PR — see [dead code D2](#9-dead-code-dispositions)) |
| 206–226 | `select_profile` | `roots.rs` | `pub` |
| 228–230 | `is_nonempty_profile` | `roots.rs` | private to `roots` |
| 232–241 | `trait BoolNot` + impl | `roots.rs` | private to `roots` |
| 243–268 | `resolve` | `roots.rs` | `pub` (re-exported; `app.rs:499`, `:513`) |
| 270–279 | `config_root` (fn) | `roots.rs` | private to `roots` |
| 281–305 | `agent_root` (fn) | `roots.rs` | private to `roots` — **isolation rule, see invariant (a)** |
| 307–313 | `session_root` (fn) | `roots.rs` | private to `roots` (name shadows the `EffectiveRoots::session_root` field — keep, it is file-local) |
| 315–324 | `struct DiscoverConfig<'a>` | `discover.rs` | `pub` (re-exported; `app.rs:523`, `tests.rs:38`) |
| 326–338 | `DiscoverConfig::new` | `discover.rs` | `pub` |
| 340–351 | `struct ImportBadge` | `format.rs` | `pub` (re-exported) |
| 353–368 | `ImportBadge::to_display` | `format.rs` | `pub` |
| 370–390 | `struct ParsedSession` | `format.rs` | `pub` (re-exported) — the shared type |
| 393–433 | `ParsedSession::into_session` | `format.rs` (impl block #1) | `pub` |
| 435–462 | `ParsedSession::resume_spec` | `resume.rs` (impl block #2) | `pub` |
| 464–474 | `resume_env` | `resume.rs` | private to `resume` |
| 476–487 | `struct DiscoverOutcome` | `discover.rs` | `pub` (re-exported) |
| 489–545 | `discover` | `discover.rs` | `pub` (re-exported; `app.rs:523`) |
| 547–555 | `iter_session_files` | `discover.rs` | private to `discover` |
| 557–579 | `collect_jsonl` | `discover.rs` | private to `discover` |
| 581–590 | `parse_session_file` | `discover.rs` | private to `discover` — **calls `format::extract_session`** |
| 592–608 | `struct TitleState` + `set` | `format.rs` | private to `format` |
| 610–721 | `extract_session` | `format.rs` | **`pub(super)` — the one required promotion** |
| 723–730 | `find_session_header` | `format.rs` | private to `format` |
| 732–781 | `is_user_attributed` | `format.rs` | private to `format` |
| 783–796 | `extract_user_message` | `format.rs` | private to `format` |
| 798–826 | `parse_import` | `format.rs` | private to `format` |
| 828–831 | `as_system_time` | `format.rs` | private to `format` |
| 833–846 | `activity_status` | `activity.rs` | `pub` (re-exported; `app.rs:531`) |
| 848–863 | `struct ActivityEvidence` | `activity.rs` | `pub` (re-exported; `tests.rs:36-43`) |
| 865–880 | `ActivityEvidence::matches` | `activity.rs` | **stays private to `activity`** |
| 882–892 | `risk_status` | `format.rs` | `pub` (re-exported; `app.rs:530`) — see [homeless items](#homeless-items) |
| 894–897 | `was_live_growing` | `discover.rs` | `pub` — **visibility unchanged by the split** (moved untouched); **deleted** in the cleanup PR together with its `pi.rs:621` twin, see [dead code D4](#9-dead-code-dispositions) |
| 899–912 | `extract_session_pub` | `format.rs` | `pub` `#[doc(hidden)]` (re-exported; `tests.rs:1080-1127`) |
| 913–917 | `parse_import_pub` | `format.rs` | `pub` `#[doc(hidden)]` (re-exported) |
| 918–923 | `is_user_attributed_pub` | `format.rs` | `pub` `#[doc(hidden)]` (moved untouched; **deleted** in the cleanup PR — see [dead code D5](#9-dead-code-dispositions)) |
| 925–926 | `#[cfg(test)] mod tests;` | `mod.rs` | — |

#### Homeless items

Two items fit none of the five names and need an explicit decision:

- **`risk_status` (882–892)** calls `scope::broad_workspace_risk`. It is not formatting, but it
  is a *derived property of a `ParsedSession`* consumed at the same call site as
  `into_session` (`app.rs:530` next to `529`). **Decision: `format.rs`**, adjacent to
  `into_session`, which is its only production co-caller. Do not create a sixth file for one
  10-line function.
- **`was_live_growing` (894–897)** reads `FileOutcome::IncompleteTail` from a `ReadResult`.
  It has zero call sites. **Decision: `discover.rs`** — that is where `ReadResult` is produced
  (`581-590`), so it lands beside its only possible input. **Move it with its `pub` visibility
  unchanged.** Its actual disposition (deletion) is a cleanup-PR concern; see
  [D4](#9-dead-code-dispositions).

> **Governing rule for the whole assignment table: the split PR moves everything untouched.**
> No renames, no deletions, no visibility demotions, no signature changes — with the single
> exception of the one required `pub(super)` promotion on `extract_session` below, which is a
> hard compile requirement rather than a judgement call. Every disposition in
> [section 9](#9-dead-code-dispositions) lands in a **separate cleanup PR**. The reason is
> reviewability: a diff that `git diff -M -C --stat` reports as near-100%-similarity renames can
> be reviewed as a pure move in minutes, whereas a diff that mixes moves with deletions and
> renames must be read line by line, which is exactly how a silent behavior change (see
> [the three invariants](#three-invariants-a-refactorer-must-not-break)) slips through.

#### Test wrappers

The `#[doc(hidden)]` wrappers at `899-923` forward into `format` internals. They **must** live in
`format.rs`; putting them anywhere else would force `extract_session`, `parse_import`, and
`is_user_attributed` to become `pub(super)` for no reason. Co-locating them is what keeps the
format internals private.

### What stays in `mod.rs`

After the split, `mod.rs` is roughly 90 lines:

1. The module doc comment (current `1-77`), **updated** per [D1](#read-only-invariant) to note
   that discovery may enumerate the process table read-only.
2. `pub const AGENT: &str = "omp";` — the one constant every submodule needs, kept at the root to
   avoid a circular-feeling `roots::AGENT`.
3. Five `mod` declarations.
4. The `pub use` re-export block.
5. `#[cfg(test)] mod tests;`

```rust
mod activity;
mod discover;
mod format;
mod resume;
mod roots;
```

All five are **private** `mod` declarations. Everything public reaches the outside world only
through the explicit re-export block, which means the public surface is auditable in one place.

### The single required visibility promotion

Exactly one item changes visibility class:

**`format::extract_session` → `pub(super)`** (currently private, `mod.rs:613`).

`parse_session_file` (`583`) lives in `discover.rs` but calls `extract_session` (`613`), which
lives in `format.rs`. `pub(super)` scopes it to the `omp` module tree and no further — it does
**not** become part of the crate or public API.

Every other cross-file need is already `pub` and stays `pub`. No other private item is promoted.

### The re-export block

```rust
pub use self::{
    activity::{ActivityEvidence, activity_status},
    discover::{DiscoverConfig, DiscoverOutcome, discover, was_live_growing},
    format::{
        ImportBadge, ParsedSession, extract_session_pub, is_user_attributed_pub,
        parse_import_pub, risk_status,
    },
    roots::{
        AGENT_DIR_NAME, DEFAULT_BASE_RELATIVE, ENV_AGENT_DIR, ENV_CONFIG_DIR, ENV_OMP_PROFILE,
        ENV_PI_PROFILE, ENV_SESSION_DIR, ENV_XDG_DATA_HOME, EffectiveRoots, PROFILES_DIR_NAME,
        PROFILE_AGENT_DIR_NAME, ProfileSelection, ResolutionInputs, resolve, select_profile,
    },
};
```

This list is the split PR's reviewer gate: it must reproduce the pre-split public surface
**exactly**, including `was_live_growing` and `is_user_attributed_pub`, both of which are `pub`
today and therefore stay `pub` through the move. The cleanup PR prunes them from this block in
the same commit that deletes them.

Plus, once wiring lands: `activity::{ActivityEvidenceMap, Breadcrumb, BreadcrumbSource,
NullBreadcrumbs, correlate_live, correlate_live_with}`.

`ParsedSession::resume_spec` needs no re-export — it is an inherent method, reachable through the
re-exported `ParsedSession` regardless of which file its `impl` block lives in. Two `impl` blocks
for one type in two files of the same crate is legal Rust; this is the mechanism that lets
`into_session` (format) and `resume_spec` (resume) separate cleanly.

### Public-API-preservation checklist

Every external reference must compile unchanged. Verify each:

| Call site | Reference | Preserved by |
|---|---|---|
| `src/app.rs:498` | `omp::ResolutionInputs::from_env()` | `roots::ResolutionInputs` re-export |
| `src/app.rs:499` | `omp::resolve(&base_inputs)` | `roots::resolve` re-export |
| `src/app.rs:503` | `omp::PROFILES_DIR_NAME` | `roots::PROFILES_DIR_NAME` re-export |
| `src/app.rs:513` | `omp::resolve(&inputs)` | `roots::resolve` re-export |
| `src/app.rs:523` | `omp::discover(&omp::DiscoverConfig::new(root.clone(), scope))` | `discover::{discover, DiscoverConfig}` re-exports |
| `src/app.rs:527` | `parsed.resume_spec(&root)` | inherent method on re-exported `ParsedSession` (impl in `resume.rs`) |
| `src/app.rs:529` | `parsed.clone().into_session(...)` | inherent method on re-exported `ParsedSession` (impl in `format.rs`) |
| `src/app.rs:530` | `omp::risk_status(&parsed, home().as_deref())` | `format::risk_status` re-export |
| `src/app.rs:531` | `omp::activity_status(&parsed, None)` | `activity::activity_status` re-export |
| `src/integration/omp/tests.rs:36-43` | `omp::{self, ActivityEvidence, DiscoverConfig, EffectiveRoots, ImportBadge, ParsedSession, ProfileSelection, ResolutionInputs}` | all eight covered by the re-export block |
| `tests/step9_app.rs:33, 106-115, 132, 263-287` | compiled-binary behavior only | no source dependency |
| `tests/picker_spike.rs:53, 586` | compiled-binary behavior only | no source dependency |

**Mechanical proof:** the split is API-preserving iff `cargo build --all-features --locked`
succeeds with **zero** changes to `src/app.rs` and **zero** changes to the `use` block at
`src/integration/omp/tests.rs:36-43`. Make that the PR4 gate — if either file needs an edit, the
re-export block is wrong.

### Three invariants a refactorer must not break

#### (a) Profile precedence and named-profile isolation

**Precedence:** `--profile` flag > `OMP_PROFILE` > `PI_PROFILE` > `Default`, implemented in
`select_profile` (`mod.rs:206-226`): flag at `210-214`, `OMP_PROFILE` at `215-219`, `PI_PROFILE`
at `220-224`, `Default` at `225`. Whitespace-only names fall through via `is_nonempty_profile`
(`228-230`), which uses the dependency-free `BoolNot` helper (`232-241`).
Guarded by `tests.rs:462` `empty_profile_flag_falls_through_to_env`.

Note that `ResolutionInputs::from_env` (`181-192`) reads `OMP_PROFILE` at `189` and `PI_PROFILE`
at `190` and leaves both flag fields `None` — the flags are set by callers.

**Isolation — the fragile one.** `agent_root` (`mod.rs:281-305`) has two branches:

- **Default** (`287-297`): `PI_CODING_AGENT_DIR` verbatim (`288-291`, with the comment at `289`
  "PI_CODING_AGENT_DIR overrides only the unprofiled agent root"), else `XDG_DATA_HOME/agent`
  (`292-295`), else `<config_root>/agent` (`296`).
- **Named** (`299-303`): returns `config_root/profiles/<name>/agent` unconditionally and **never
  reads `agent_dir_env` or `xdg_data_home`.**

> **The isolation rule is enforced BY OMISSION, not by a guard.** There is no `if` rejecting the
> env vars in the named branch — they are simply not referenced. The intent is stated only in the
> comment at `298` ("Named profiles deliberately ignore PI_CODING_AGENT_DIR and XDG_DATA_HOME")
> and the module doc at `20-21`.
>
> A well-meaning refactor that unifies the two branches — "both compute an agent root, let's
> factor out the common env lookup" — **silently breaks profile isolation with no compiler
> error.** The only defense is three tests: `tests.rs:341`
> `named_profile_ignores_agent_dir_env`, `tests.rs:438`
> `xdg_data_home_ignored_for_named_profiles`, and `tests.rs:327`
> `agent_dir_env_overrides_unprofiled_agent_root_only`.
>
> **When `agent_root` moves to `roots.rs`, move the comment at `298` with it and add a
> `// DO NOT unify these branches — see docs/design/omp-active-detection-plan.md` marker.**

Isolation reaches session identity: `SessionKey` is built at `mod.rs:418-423` as
`{ agent, effective_root: session_root, profile: profile.as_profile_field(), native_locator }`,
so identical native IDs in different profiles cannot collide
(`tests.rs:846`, `tests.rs:901`).

`config_root` (`270-279`): `PI_CONFIG_DIR` wins, else `$HOME/.omp`, else `None`
(`tests.rs:456`). `session_root` (`307-313`): `--session-dir` → `(flag, true)`, else
`(agent_root, false)`.

#### (b) Title resolution behavior

See [section 7](#7-title-priority-adjudication). The implementation is **positional,
latest-non-empty-wins**, not a fixed source ranking. Reordering the match arms at `638-671`
silently changes output with no compiler error. The six tests at `tests.rs:542-644` are the
guardrail.

#### (c) Resume argv construction order

`ParsedSession::resume_spec` (`mod.rs:435-462`) builds argv in a strict order that OMP's CLI
depends on:

| Line | Action |
|---|---|
| 436 | `let mut argv: Vec<OsString> = Vec::with_capacity(6);` |
| 437–440 | **profile flag FIRST**: `if let ProfileSelection::Named(name) = &roots.profile { push("--profile"); push(name.clone()); }` |
| 441–442 | `push("--resume"); push(self.id)` → `omp --resume <id>` or `omp --profile <name> --resume <id>` |
| 443–446 | appends `--session-dir <session_root>` **only when** `roots.custom_session_root` |
| 447 | `cwd = self.workspace.clone().unwrap_or_else(|| PathBuf::from("."))` |
| 449–450 | rationale comment for `resume_env` |
| 451 | `env = resume_env(roots)` |
| 453–458 | `ResumeSpec { program: AGENT, argv, cwd, env }` |

`resume_env` (`464-474`) emits `PI_CONFIG_DIR` **only** when `roots.config_root_overridden`
(`466-472`, set at `mod.rs:262` from `config_dir_env.is_some()`), else an empty env (`473`).
Injecting the default root would change OMP's native resume lookup relative to direct invocation.

No shell is involved (doc at `433`); `src/launch.rs:197` runs `Command::new(&spec.program)`
directly. **Moving this to `resume.rs` must preserve the line order exactly.** Guarded by
`tests.rs:1210, 1233, 1261, 1290, 1307, 1335` and the fake-binary provenance tests at
`tests.rs:1618`, `:1652` (`fake_omp` at `:1569`, `run_resume_spec_capturing` at `:1589`, both
`#[cfg(unix)]`).

Note: no `--profile` or `--session-dir` CLI flag exists on `resume` itself
(`rg -i profile src/cli.rs` → no matches), so `session_dir_flag` is always `None` and
`custom_session_root` always `false` in production. The `443-446` branch is exercised only by
tests today — do not delete it as dead code; it is the contract for a future flag.

### Per-file imports after the split

| File | Imports |
|---|---|
| `roots.rs` | `std::{ffi::OsString, path::{Path, PathBuf}}` only. No `serde_json`, no `jsonl`. **The cleanest cut in the file.** |
| `discover.rs` | `std::{fs, io, path}`, `crate::jsonl::{self, Bounds}`, `crate::scope::Scope`, `super::{format, roots::EffectiveRoots}` |
| `format.rs` | `serde_json::Value`, `crate::jsonl::ReadResult`, `crate::message::{self, UserMessage}`, `crate::summary::{summarize_texts, default_width}` (currently fully-qualified at `706-707`), `crate::time::json_value_to_system_time` (`830`), `crate::scope`, `crate::session::{Session, SessionKey, SupportStatus, WorkspaceEvidence, RiskStatus, ActivityStatus}` |
| `resume.rs` | `crate::session::ResumeSpec`, `std::ffi::OsString`, `super::roots::{EffectiveRoots, ProfileSelection, ENV_CONFIG_DIR}` |
| `activity.rs` | `crate::session::ActivityStatus`, `crate::proc::ProcessTable` (after PR2), `std::{collections::HashMap, ffi::{OsStr, OsString}, path::{Path, PathBuf}, time::SystemTime}` |

---

## 7. Title priority adjudication

**The code is authoritative. The task brief's stated priority is inverted relative to the
implementation. This is a documentation bug in the brief, not a code bug.**

### What the code does

`extract_session` (`mod.rs:610-721`) runs a **single pass over records in file order**
(`632-699`). Three arms can set the title:

| Lines | Record type | Sets title from |
|---|---|---|
| 638–645 | `type == "session"` (v3 header) | `record["title"]` |
| 651–660 | `type == "title"` (leading sidecar) | `record["title"]`, falling back to `record["text"]` (`655-656`) |
| 663–671 | `type == "title_change"` | `record["title"]` |

All three call `TitleState::set` (`599-608`), which **overwrites** whenever the incoming value is
`Some` and non-blank after trimming. There is no ranking, no precedence check, no "first wins".
**Later non-empty wins, unconditionally.**

The comments at `634-637` and `647-650` say so explicitly: *"we process in record order so a
later `title_change` can override it."*

### Net effective precedence

Combined with OMP's real on-disk layout — the title sidecar **precedes** the v3 header
(`mod.rs:31-33`) — the net effective precedence is:

> **last `title_change` > v3 header `title` > leading `title` sidecar**

### The discrepancy

The task brief describes the priority as *"leading title record > v3 header > title_change"* —
**exactly reversed**.

Both statements describe the same file, from different angles:

- The brief's ordering is the **file layout order**: the sidecar appears first in the file, then
  the header, then any `title_change` records.
- The code's ordering is the **precedence order**: last writer wins, so the same list read
  backwards.

The brief's phrasing conflates "appears first" with "takes priority". **The code's behavior is
correct and must be preserved verbatim.** The brief's phrasing should be corrected wherever it
is repeated.

Fallback when no arm ever fires: `crate::summary::summarize_texts(texts,
crate::summary::default_width())` at `705-708`.

Import badges are overlaid **after** title resolution, in `into_session` at `410-414` — they
decorate the resolved title and do not participate in precedence.

### What `format.rs` must preserve

The six points a refactorer must not touch:

1. **Record-order iteration.** The single loop at `632-699` must stay a single forward pass. Do
   not collect candidates and rank them.
2. **Arm order within the loop** (`638-645`, `651-660`, `663-671`). Reordering the `match` arms
   changes which one wins for a record that could satisfy two — silently.
3. **`TitleState::set` overwrite semantics** (`599-608`): overwrite iff incoming is `Some` and
   non-blank after `trim`. Do not change to "set only if currently empty".
4. **The `title` → `text` key fallback** at `655-656`, which applies **only** to the sidecar arm,
   not to the header or `title_change` arms.
5. **The `summarize_texts` fallback** at `705-708` fires only when no arm ever set a non-blank
   title.
6. **Import-badge overlay stays in `into_session`** (`410-414`), i.e. in `format.rs` but
   *outside* `extract_session`. Do not fold it into the title loop.

### Regression tests that guard it

`src/integration/omp/tests.rs:542-644` — six tests:

- `tests.rs:547`, `:564`, `:580`, `:597`, `:615`, `:633`

Plus the header-parsing group `tests.rs:476-541` (three tests, title-before-header ordering,
including `:521`) and the timestamp-fallback test at `tests.rs:1080-1127` which drives
`extract_session_pub` directly.

**Run these nine tests as the acceptance gate for any change to `format.rs`.** They are the only
thing standing between a plausible-looking refactor and silently wrong titles.

---

## 8. Test split plan

`src/integration/omp/tests.rs` is 1733 lines / 60 tests. Convert to a directory module mirroring
the source split.

```
src/integration/omp/
├── mod.rs                 (thin; `#[cfg(test)] mod tests;` at :925-926 unchanged)
├── activity.rs
├── discover.rs
├── format.rs
├── resume.rs
├── roots.rs
└── tests/
    ├── mod.rs             ← shared Fixture + record builders + submodule decls
    ├── activity.rs
    ├── discover.rs
    ├── format.rs
    ├── resume.rs
    └── roots.rs
```

### Section-to-file mapping

| Source lines | Section (tests) | → `tests/` file |
|---|---|---|
| 34–43 | `use` block | `tests/mod.rs` (unchanged text — see the [preservation checklist](#public-api-preservation-checklist)) |
| 46–294 | Helpers: `Fixture` (50–183) + record builders (185–293) | **`tests/mod.rs` — shared by all five** |
| 296–475 | ROOT RESOLUTION (11) | `tests/roots.rs` |
| 476–541 | HEADER PARSING: title-before-header (3) | `tests/format.rs` |
| 542–644 | TITLE RESOLUTION (6) | `tests/format.rs` |
| 645–758 | USER MESSAGE EXTRACTION + attribution (5) | `tests/format.rs` |
| 759–840 | FOREIGN SESSION IMPORT (2) | `tests/format.rs` |
| 841–1013 | DUPLICATE IDs ACROSS PROFILES (4) | `tests/discover.rs` (exercises `roots` too; discovery is the driver) |
| 1014–1079 | SCOPE FILTERING (3) | `tests/discover.rs` |
| 1080–1127 | TIMESTAMP FALLBACK CHAIN (1, uses `extract_session_pub`) | `tests/format.rs` |
| 1128–1204 | MALFORMED / TRUNCATED / EMPTY (4) | `tests/discover.rs` |
| 1205–1348 | `ResumeSpec` (6) | `tests/resume.rs` |
| **1349–1441** | **ACTIVITY (2): `activity_unknown_without_evidence` :1354, `activity_active_only_with_live_process_tty_and_matching_breadcrumb` :1373** | **`tests/activity.rs`** |
| 1442–1492 | SESSION CONSTRUCTION + RISK (2) | `tests/format.rs` |
| 1493–1560 | READ-ONLY INVARIANT (3) | `tests/discover.rs` |
| 1561–1695 | FAKE `omp` LAUNCH PROVENANCE (`fake_omp` :1569, `run_resume_spec_capturing` :1589, tests :1618, :1652) | `tests/resume.rs`, `#[cfg(unix)]` |
| 1696–1733 | import badge display (3) | `tests/format.rs` |

### The `pub(super)` sharing mechanism

`Fixture` (`50-183`) and the record builders (`185-293`) are used by every section. They move to
`tests/mod.rs` and are promoted from private to **`pub(super)`**:

```rust
// src/integration/omp/tests/mod.rs
use serde_json::{Value, json};

use crate::{
    integration::omp::{
        self, ActivityEvidence, DiscoverConfig, EffectiveRoots, ImportBadge, ParsedSession,
        ProfileSelection, ResolutionInputs,
    },
    scope::{Direction, Scope},
    snapshot,
};

mod activity;
mod discover;
mod format;
mod resume;
mod roots;

pub(super) struct Fixture { /* body unchanged from tests.rs:50-183 */ }

impl Fixture {
    pub(super) fn new() -> Self { /* ... */ }
    // every method used across sections promoted to pub(super)
}

pub(super) fn session_header_record(/* ... */) -> Value { /* :185-293 unchanged */ }
pub(super) fn title_record(/* ... */) -> Value { /* ... */ }
pub(super) fn user_message_record(/* ... */) -> Value { /* ... */ }
// etc.
```

Each submodule then starts with:

```rust
// src/integration/omp/tests/format.rs
use super::*;
```

`pub(super)` from `tests/mod.rs` means "visible to `omp`", and since `tests/*.rs` are children of
`tests/mod.rs` (which is itself a child of `omp`), `use super::*` from a sibling reaches them.
Nothing escapes the `omp` module tree, and nothing is `pub` beyond `#[cfg(test)]` code.

**Do not change the `use` block at `tests.rs:36-43` when moving it.** It is the mechanical proof
that the source split preserved the public API; editing it invalidates that proof.

### New tests added by the wiring PRs

| File | Tests |
|---|---|
| `src/proc.rs` (inline `#[cfg(test)] mod tests`) | `parse_ps_output` against captured fixtures: normal rows, `??` rows, unresolvable `tdev`, malformed rows, empty output, trailing whitespace in `ucomm` |
| `src/proc.rs` | `DeviceMap::scan` against a `tempfile` dir containing regular files and (where creatable) char devices; unreadable dir → empty map |
| `tests/activity.rs` | `correlate_live_with` using a stub `BreadcrumbSource`: no processes → empty map; process with no TTY → empty; TTY with no breadcrumb → empty; breadcrumb naming a nonexistent path → empty; full match → one entry |
| `tests/activity.rs` | `ActivityEvidenceMap::for_transcript`: canonical hit, lexical-fallback hit (symlinked dir), miss |
| `tests/activity.rs` | `NullBreadcrumbs` yields an empty map for any process table (the "degrades to today" proof) |

The two existing activity tests (`:1354`, `:1373`) move **unchanged**. They test
`activity_status`/`matches`, which this plan does not modify.

---

## 9. Dead code dispositions

The split will surface five items that are pub-but-unused or misnamed. Decide each explicitly
rather than letting the refactor quietly delete or preserve them.

> **Governing rule: the split PR (PR4) moves all five untouched. Every disposition below lands in
> a separate cleanup PR (PR6).** This is deliberate and it overrides the temptation to "do it
> while the file is already moving". A split diff that `git diff -M -C` reports as near-100%
> renames is reviewable as a pure move; the moment a rename or a deletion is mixed in, the
> reviewer has to read every line to convince themselves nothing else changed, and that is
> precisely how a silent behavior regression gets waved through. The cleanup PR's own
> verification is trivial: `cargo build --all-features --locked` — if it compiles, nothing
> referenced the deleted items.

| # | Item | Location | Disposition | Rationale |
|---|---|---|---|---|
| **D1** | `ResolutionInputs::with_profile_flag` | `mod.rs:195-198` | **DELETE** (cleanup PR) | Zero call sites. `src/app.rs:510-512` sets `inputs.profile_flag` by **direct field access** and deliberately does *more* — it also nulls `omp_profile_env` and `pi_profile_env`. So the builder is not merely unused, it is **already semantically diverged**: a future caller who "adopts" it would silently reintroduce env-precedence leakage into profile resolution. Deleting is safe: every `ResolutionInputs` field is `pub` (`mod.rs:159-173`) and tests already construct by struct literal (`tests.rs:175-177`). **Delete pi's twin at `pi.rs:126` in the same commit** so the two integrations do not drift. |
| **D2** | `ResolutionInputs::with_session_dir_flag` | `mod.rs:201-204` | **DELETE** (cleanup PR) | Same reasoning as D1, plus a stronger one: it is **unreachable by construction**. No `--session-dir` CLI flag exists on `resume` (`rg -i profile src/cli.rs` → no matches), so `session_dir_flag` is unconditionally `None` and `custom_session_root` unconditionally `false` in production. A builder for a value that cannot be supplied is not a forward-looking API, it is a stub — and when the flag does arrive, `inputs.session_dir_flag = Some(..)` is one line. Note this does **not** justify deleting the `443-446` branch in `resume_spec`, which is the real argv contract and is exercised by tests. |
| **D3** | `ENV_SESSION_DIR = "--session-dir"` | `mod.rs:88` | **RENAME → `SESSION_DIR_FLAG`** (cleanup PR) | Two independent reasons. (1) It is flat-out misnamed: the `ENV_` prefix says environment variable, but the value is a **CLI flag string**. Every sibling constant (`ENV_CONFIG_DIR:84`, `ENV_AGENT_DIR:86`, `ENV_PI_PROFILE:90`, `ENV_OMP_PROFILE:92`, `ENV_XDG_DATA_HOME:104`) really is an env var. (2) Decisively: it **collides in meaning with `pi::ENV_SESSION_DIR` (`pi.rs:54`), which *is* a real env var** (`PI_CODING_AGENT_SESSION_DIR`) and *is* actually read at `pi.rs:113`. Two identically-named public constants with opposite meanings across sibling integrations under the same `crate::integration` parent is an active trap for anyone reading both files. Zero references, so the rename is free and strictly clarifying. **Rename, do not delete** — the string is still the correct token for `resume_spec` to emit at `443-446`. **Direction matters: `SESSION_DIR_FLAG`, not `FLAG_SESSION_DIR`** — noun-then-role reads naturally and sorts beside the `SESSION_*` items rather than inventing a `FLAG_*` namespace with one member. |
| **D4** | `was_live_growing` | `mod.rs:894-897` | **KEEP + MOVE untouched to `discover.rs` in the split PR; DELETE in the cleanup PR** | Zero call sites in either integration. The tempting "plausible future liveness signal" argument does not survive inspection: `FileOutcome::IncompleteTail` is **already surfaced through the real diagnostics path** by `claude/mod.rs:485` and `codex/mod.rs:466`, so the predicate is genuinely redundant, not merely un-adopted. And this plan's design explicitly does *not* use transcript growth as evidence (see [the `breadcrumb_alive` definition](#definition-of-breadcrumb_alive)) — process + breadcrumb + start-time correlation is the whole contract. **Delete pi's twin at `pi.rs:621` in the same symmetric commit**; deleting one and keeping the other would manufacture an omp/pi asymmetry that the next reader has to explain. Fallback if a reviewer objects pre-1.0: `#[doc(hidden)]` + `// TODO(#N): adopt or remove`, but prefer the deletion. |
| **D5** | `is_user_attributed_pub` | `mod.rs:918-923` | **KEEP + MOVE untouched to `format.rs` in the split PR; DELETE in the cleanup PR** | The only `#[doc(hidden)]` test wrapper with **no caller at all**. Its two siblings are live: `extract_session_pub` (`904`) at `tests.rs:1112`/`:1123`, `parse_import_pub` (`913`) at `tests.rs:1726`. Unlike D4 it has **no pi twin** (`pi.rs` exposes only `extract_session_pub` at `:632`), so deleting it is asymmetry-free. It exists solely to widen a private function's visibility for a test that was never written, and attribution is already covered by the five tests at `tests.rs:645-758`. After the test split, `tests/format.rs` is a descendant of `omp` and can reach `format::is_user_attributed` through `pub(super)` directly if it is ever actually needed. |

**Net cleanup-PR diff: 1 rename, 5 deletions** — three in omp (`with_profile_flag`,
`with_session_dir_flag`, `is_user_attributed_pub`) plus `was_live_growing`, and two in pi
(`with_profile_flag`, `was_live_growing`). Prune the corresponding entries from the `pub use`
block in the same commit.

**Do none of this in the split PR.** They are pure hygiene; mixing them into a move-only diff
destroys the one property that makes the move reviewable.

---

## 10. Risk register

### R1 (TOP) — a false-positive `Active` makes `--no-confirm` block on stdin

This outranks everything else in this document and deserves prose rather than a table cell.

`src/launch.rs:143-147` pushes `"Session is Active"` into `risk_reasons` whenever
`matches!(session.activity, ActivityStatus::Active { .. })`. Then `src/launch.rs:161-163`:

```rust
let risky = !risk_reasons(session, false).is_empty();
risky || (confirm_always && !no_confirm)
```

**`risky` short-circuits the entire `no_confirm` term.** The doc comment at `launch.rs:160` is
explicit that this is intentional: *"Risk confirmations are mandatory: `no_confirm` suppresses
only ordinary confirmation."* So `resume --no-confirm` against a *falsely* Active session drops
into `launch::confirm`, which reads `io::stdin()` (`app.rs:280-284`), and **hangs a script**.
Secondarily, `status_label` (`app.rs:693-698`) flips `READY` → `ACTIVE` in `--list` output.

**Why this outranks every other risk here:** every other risk in this register degrades to
`Unknown`, i.e. exactly today's behavior. This one is the only risk that makes the tool *worse
than not shipping the feature at all*, and it is triggered by a single wrong bit.

**Mitigations, layered:**

1. **Every PR before the real breadcrumb reader is PROVABLY incapable of producing `Active`.**
   `NullBreadcrumbs::breadcrumbs()` returns an empty `Vec`, so no `ActivityEvidence` is ever
   constructed, so `ActivityEvidenceMap` is always empty, so `for_transcript` always returns
   `None`, so `activity_status` hits the `_ => ActivityStatus::Unknown` arm at `mod.rs:844`.
   Even if evidence *were* constructed with `breadcrumb_alive: false`, `matches` short-circuits
   at **`mod.rs:869-871`** before any path comparison. The wiring ships provably no-op — this is
   a structural property, not a test result, and it is what makes the whole staged approach safe.
2. **The gate itself is narrow.** `mod.rs:869-871` requires live process **and** TTY **and**
   alive breadcrumb; `872-877` additionally requires exact canonical path equality. A false
   positive requires *simultaneously*: a live process whose command basename is exactly `omp`,
   occupying a TTY, **and** a breadcrumb for that same TTY, fresh relative to that process's
   start time, pointing at that *exact* transcript file. A coincidentally-named binary cannot
   fabricate the breadcrumb.
3. **The recycled-TTY case is closed by the start-time rule.** The residual false-positive
   surface after (2) is breadcrumb staleness on a reused TTY, which
   `recorded_at >= started_at` ([D3](#definition-of-breadcrumb_alive)) is specifically designed
   to eliminate.
4. **The real breadcrumb reader ships behind `RESUME_DISABLE_PROC_PROBE`**, which is in place
   from the wiring PR onward, so an operator can turn the whole probe off without a release.
5. **Target: no `Active` verdict without all three signals, and prefer a false negative
   (`Unknown`) every single time.** That is already the documented contract (`README.md:153`,
   `mod.rs:53-54`). A false negative costs nothing — it is today's behavior.
6. **Escalate, do not silently fix:** *should `--no-confirm` be allowed to bypass an `Active`
   confirmation?* Today it cannot. Changing `launch.rs:161-163` is **out of scope** for this
   plan and must be a separate product decision, taken before the real breadcrumb reader flips
   on, not as a drive-by.

**Acceptance:** the real-breadcrumb PR must not merge without the probe having confirmed the
exact breadcrumb→transcript mapping, and without a manual check of the confirm path against a
genuinely active session **and** a recently-exited one.

### The rest

| # | Risk | Severity | Mitigation |
|---|---|---|---|
| **R2** | **O(N) or O(N×profiles) subprocess explosion.** Computing evidence in the closure at `src/app.rs:526-536` — the natural spot, since `531` is where the `None` lives — runs one `ps` per session, multiplied by profiles because the closure is nested in the `for root in roots` loop at `522`. At 40 ms, 200 sessions × 3 profiles = 24 s. Identical trap at `app.rs:400-413` for Pi. | **High** | Snapshot at `app.rs:74-81`, once, before fan-out — see [why this is not O(N)](#why-this-is-not-on-subprocesses). Add a PR3 review-checklist item: "no `proc::snapshot()` call inside any loop or closure." Consider a debug assertion counting invocations per process. |
| **R3** | **Secondary trap: one snapshot per agent thread.** Hoisting only to the top of `discover_omp` (`app.rs:498`) still gives up to 2 concurrent `ps` runs (omp + pi), since `app.rs:166`/`204` spawn one thread per agent. | **Medium** | Same fix: construct `DiscoveryContext` at `app.rs:74-81` and clone the `Arc` per thread at `166-178` and `204-217`, exactly as `Arc<Scope>` is cloned at `169`/`207`. |
| **R4** | **The probe sits outside cancellation.** The `CancelToken` checks at `app.rs:333` and `:219` are per-agent-thread; `JOIN_BUDGET` (`src/runtime.rs:23`, 250 ms) governs worker joins. The probe runs before any thread exists, so it is covered by neither, and it is on the critical path for picker start-up. | **Medium** | `PROC_PROBE_BUDGET = 300 ms` (derivation: ~7× the measured 40 ms median — see [that section](#proc_probe_budget-and-degradation-paths)) enforced by the probe itself via `recv_timeout` on a helper thread; on expiry return `ProcessTable::empty()` and leave the helper detached to reap the child (`runtime.rs:7-9` philosophy). Skip the probe entirely when neither `omp` nor `pi` is in `options.agents` (`app.rs:106`). |
| **R5** | **The breadcrumb location is unknown.** `breadcrumb_session_path` (`mod.rs:860`) is consumed but never produced; `docs/research/session-formats.md:127` asserts breadcrumbs exist without a path. A guessed path yields zero evidence — indistinguishable from correct behavior, and no test would catch it. | **High** | `NullBreadcrumbs` is the shipped default. The 7-step [`omp-breadcrumb-probe`](#task-omp-breadcrumb-probe) is PR0 and gates PR5. **Never merge a guessed path.** |
| **R6** | **`ActivityEvidence` derives only `Clone, Debug`** (`mod.rs:851`) while sibling OMP types derive `Eq`/`PartialEq`. Any correlation code that wants to compare or dedupe evidence, or any test wanting `assert_eq!`, will not compile. | **Low** | Add `PartialEq, Eq` in PR3. It is a strictly additive derive on a struct of `bool`/`Option<OsString>`/`PathBuf`/`SystemTime`, all of which are `Eq`. No behavior change. |
| **R7** | **The read-only-invariant doc becomes misleading.** `mod.rs:57` says the module "never invokes OMP during discovery/preview". Running `ps` is not invoking OMP, but the wording invites a reviewer objection and a future author's confusion. `src/snapshot.rs:145` `assert_unchanged` is unaffected — `ps` mutates nothing. | **Low** | PR2 updates the doc comment at `mod.rs:45-59` to state that discovery may enumerate the OS process table read-only and still never executes the OMP binary. |
| **R8** | **`tests/step9_app.rs` hermeticity.** It drives the **compiled binary** (`env!("CARGO_BIN_EXE_resume")`, `:12`) against tempdir fixtures (`:106-115` default-root OMP discovery, `:263-287` import badge) and asserts on `--list` output, where `status_label` (`app.rs:693-700`) prints `"ACTIVE"` vs `"READY"`. Once `run()` probes the **host** process table, a developer with a real `omp` running makes discovery non-deterministic. | **Medium** | **Required change in the wiring PR:** set `RESUME_DISABLE_PROC_PROBE=1` in the `Command` env of the `run_with_env` helper at `tests/step9_app.rs:25-30` (which `run` at `:21` delegates to), so every step9 invocation gets `ProcessTable::empty()`. Check `tests/picker_spike.rs:53`, `:586` for the same need. **No existing assertion needs relaxing** — with the probe disabled no session can be `Active`. Add one new test asserting the probe is off by default under test and that OMP rows render `READY`. The byte-identical `--list` acceptance gate catches any regression here immediately. |
| **R9** | **Cross-platform: `tdev` is BSD-only.** procps-ng (the `ubuntu-latest` runner, `.github/workflows/ci.yml:31`) has no `tdev` column, and the `#[cfg(target_os)]` argv split means **the Linux path is untested on the development machine**. Additionally the `x86_64/aarch64-unknown-linux-musl` release targets (`ci.yml:104-110`) may run in minimal containers with no `ps` at all. | **Medium** | Per-OS cfg-gated argv (see [exact argv](#exact-argv-per-os-and-cfg-gated)): `tty=,comm=` on Linux, where the name is already free. `ErrorKind::NotFound` → empty table → `Unknown`, which is acceptable and diagnosed. **The `src/proc.rs` PR must be verified on the CI Linux runner, not only locally** — make that an explicit merge condition, not an afterthought. |
| **R10** | **`Command::new("ps")` uses PATH lookup.** A shadowed `ps` on `$PATH` could return arbitrary output. | **Low** | Precedent is `Command::new("git")` (`scope.rs:174`), and CI's fake-binary dir (`ci.yml:56-63`) contains only `pi`/`claude`/`codex`/`omp`, so `ps` is not shadowed there. The parser is total — unparseable rows are skipped and counted — so the worst case is a wrong or empty table, which lands on `Unknown`. **Accepted and noted, not mitigated.** |

---

## 11. Implementation checklist

Eight independently mergeable PRs. Each states its own verification. **Every PR must pass the
project's standard gate** (`CONTRIBUTING.md:19-21`, mirrored in `.github/workflows/ci.yml:64-66`):

```bash
cargo fmt --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --all-features --locked
```

**On the ordering.** This plan lands the *acquisition* half first (`src/proc.rs`, then the
context, then the inert wiring) and the *module split* after. The reverse — split first, then
build on the split files — is equally defensible and has one real advantage: the wiring PRs would
then touch a ~200-line `activity.rs` instead of a 926-line `mod.rs`. The order below is chosen
instead because the split PR's reviewer gate is *"`git diff --stat main -- src/app.rs` is
empty"*, and that gate is strongest when `app.rs` has already stopped moving. **Either order
works; do not mix them.** Two hard constraints survive any reordering:

1. The **module-split PR moves everything untouched** — no renames, no deletions, no visibility
   demotions. Dead-code dispositions are their own PR ([section 9](#9-dead-code-dispositions)).
2. The **wiring PR carries the byte-identical `--list` acceptance gate**. If output differs from
   `main`, the "provably no-op" property is violated and the PR must not merge.

### PR0 — `omp-breadcrumb-probe` (investigation, no code)

Run the 7-step checklist in [section 5](#task-omp-breadcrumb-probe). Deliverable: an amendment to
`docs/research/session-formats.md` pinning the breadcrumb path template, format, TTY key form,
staleness behavior, and per-profile vs global scope.

- Blocks: **PR5 only.** PR1–PR4 and PR6–PR7 proceed in parallel. If OMP is not installed, this
  task legitimately returns *blocked* — PR1–PR4 still ship unchanged, still `Unknown`
  everywhere, zero regression.
- Verification: documentation review. No code changes, so the standard gate is trivially green.

### PR1 — `src/proc.rs`, standalone

Add `src/proc.rs` per [D2](#d2--the-new-srcprocrs) and `pub mod proc;` to `src/lib.rs`. Add
`PROC_PROBE_BUDGET` to `src/runtime.rs` beside `JOIN_BUDGET` (`:23`). Includes the cfg-gated
per-OS argv, the macOS `/dev` major/minor map, the `etime` parser, `normalize_tty`, the
`live_on_tty` O(1) index, `from_entries`, and `RESUME_DISABLE_PROC_PROBE` honoring. Not called
from anywhere yet.

```bash
cargo fmt --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --all-features --locked proc::
cargo test --all-features --locked runtime::
cargo build --all-features --locked
cargo deny check licenses bans advisories sources   # MUST be a no-op: zero new deps
```

- Unit tests: `parse_ps_output` against captured fixtures (normal / `??` / unresolvable `tdev` /
  malformed / empty / trailing whitespace in `ucomm`); `DeviceMap::scan` against a `tempfile`
  dir; `etime` parsing table (`"00:01"`, `"03-15:37:25"`, garbage); `normalize_tty`
  (`/dev/ttys009` → `ttys009`, `pts/3` → `pts/3`, `?` → `None`); `from_entries` +
  `live_on_tty`; `snapshot()` finds `std::process::id()` in its own output; `snapshot()` returns
  empty when `RESUME_DISABLE_PROC_PROBE` is set; `snapshot()` completes inside
  `PROC_PROBE_BUDGET`.
- **Also re-measure on the target platform** and record the numbers in the module doc:
  `ps -Ao pid=,tdev=,etime=,ucomm=` vs `ps -Ao pid=,tty=,etime=,comm=`. Re-derive
  `PROC_PROBE_BUDGET` from the new median if it differs materially from 40 ms.
- **Merge condition: verified on the CI Linux runner, not only on macOS** ([R9](#the-rest)).
  The `tty=,comm=` argv path has no local coverage.
- Zero behavior change: nothing calls it.

### PR2 — `DiscoveryContext` + probe placement, no consumers

Add `DiscoveryContext` to `src/app.rs`, construct it at `app.rs:74-81`, thread it through
`discover_all` (`155`), `run_interactive` (`193`), `discover_agent` (`326`), `discover_pi` (`383`,
unused), `discover_omp` (`497`, unused). Drain `diagnostics` into `DiscoveryState.errors`. Update
the `mod.rs:45-59` doc per [R7](#10-risk-register).

```bash
cargo fmt --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --all-features --locked
cargo test --all-features --locked --test step9_app
```

- Zero behavior change: `app.rs:531` still passes `None`.
- **Review checklist item: no `proc::snapshot()` call inside any loop or closure** ([R2](#the-rest)).

### PR3 — activity correlation wired, `NullBreadcrumbs` default

Add `Breadcrumb`, `BreadcrumbSource`, `NullBreadcrumbs`, `ActivityEvidenceMap`, `correlate_live`,
`correlate_live_with` and `BREADCRUMB_FRESHNESS` to the OMP module, and re-export them from
`mod.rs`. Add `PartialEq, Eq` to `ActivityEvidence` ([R6](#the-rest)). Build `live` before the
loop at `app.rs:522`. **Replace the `None` at `app.rs:531`.** Set `RESUME_DISABLE_PROC_PROBE=1`
in the `run_with_env` helper at `tests/step9_app.rs:25-30` and check `tests/picker_spike.rs:53`,
`:586` ([R8](#the-rest)).

```bash
cargo fmt --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --all-features --locked
cargo test --all-features --locked --test step9_app
cargo test --all-features --locked --test picker_spike
```

**Acceptance gate — byte-identical `--list` output vs `main`, with AND without the kill switch:**

```bash
git stash && cargo build --locked --all-features && ./target/debug/resume --list > /tmp/before.txt
git stash pop && cargo build --locked --all-features
./target/debug/resume --list > /tmp/after.txt
RESUME_DISABLE_PROC_PROBE=1 ./target/debug/resume --list > /tmp/after_off.txt
diff /tmp/before.txt /tmp/after.txt        # MUST produce no output
diff /tmp/before.txt /tmp/after_off.txt    # MUST produce no output
```

Also diff `--json` output, which is stricter (it carries fields `--list` elides).

**If either diff is non-empty the "provably no-op" property has been violated — do not merge.**
`NullBreadcrumbs` returns an empty `Vec`, so no evidence can exist and every session must still
report `Unknown`; a difference means the plumbing itself changed behavior.

- **Still zero user-visible change.** That is the point: the plumbing lands and is provably inert.
- New tests per [section 8](#new-tests-added-by-the-wiring-prs), plus: `correlate_live_with` with
  a fake `BreadcrumbSource` and a `ProcessTable::from_entries` → produces `Active`; empty table
  → `Unknown`; `NullBreadcrumbs` → `Unknown`; TTY matches but path differs → `Unknown`; stale
  breadcrumb (`recorded_at < started_at`) → `Unknown`.

### PR4 — module split + test split (**pure moves only**)

Split `mod.rs` into `roots.rs` / `discover.rs` / `format.rs` / `resume.rs` / `activity.rs` per
[section 6](#6-module-split-plan). Split `tests.rs` into `tests/` per
[section 8](#8-test-split-plan). Add the two new title regression tests from
[section 7](#7-title-priority-adjudication) to `tests/format.rs`. Add the
`// DO NOT unify these branches` marker at `agent_root`'s `Named` branch.

**Apply NO dead-code dispositions here** — they are PR6. The only permitted non-move change is
the single required `format::extract_session` → `pub(super)` promotion.

```bash
cargo fmt --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo build --all-features --locked
cargo test --all-features --locked
cargo test --all-features --locked integration::omp
cargo test --all-features --locked --test step9_app
git diff --stat main -- src/app.rs                       # MUST be empty
git diff main -- src/integration/omp/tests.rs            # use block at :36-43 MUST be unchanged
git diff -M -C --stat                                    # rename detection: near-100% similarity
```

**Acceptance gate — byte-identical `--list` output:**

```bash
git stash && cargo build --locked --all-features && ./target/debug/resume --list > /tmp/before.txt
git stash pop && cargo build --locked --all-features && ./target/debug/resume --list > /tmp/after.txt
diff /tmp/before.txt /tmp/after.txt   # MUST produce no output
```

Also diff `--json` output, which is stricter (it carries fields `--list` elides).

**Reviewer gate:** the `pub use` block must reproduce
[the re-export list](#the-re-export-block) exactly — including `was_live_growing` and
`is_user_attributed_pub`, which are `pub` today and stay `pub` through the move. `git diff` must
contain no logic edits.

**Test count check** (the two new title tests from [section 7](#7-title-priority-adjudication)):

```bash
cargo test --all-features --locked integration::omp -- --list | wc -l   # expect 60 + 2 = 62
```

**Invariant tests that must pass unchanged** (see [the three invariants](#three-invariants-a-refactorer-must-not-break)):

- `tests.rs:327`, `:341`, `:438`, `:456`, `:462` — profile precedence + named-profile isolation
- `tests.rs:547`, `:564`, `:580`, `:597`, `:615`, `:633`, `:521` — title resolution
- `tests.rs:1210`, `:1233`, `:1261`, `:1290`, `:1307`, `:1335`, `:1618`, `:1652` — resume argv
- `tests.rs:846`, `:901` — cross-profile `SessionKey` collision

### PR5 — real breadcrumb reader (**gated on PR0**)

Replace `NullBreadcrumbs` with `OmpBreadcrumbs` implementing the format PR0 discovered. If PR0
found breadcrumbs are global rather than per-profile, hoist the read out of the `for root in
roots` loop at `app.rs:522` (step 7 of the probe).

```bash
cargo fmt --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --all-features --locked
cargo test --all-features --locked integration::omp::tests::activity
```

- **This is the first PR with user-visible behavior change.** The `RESUME_DISABLE_PROC_PROBE`
  kill switch is already in place from PR1 and already set by the integration-test harness from
  PR3 ([R8](#the-rest)); document it in `README.md` beside the row at `:153`.
- **Manual acceptance, all four directions:**
  1. Start an interactive OMP session. Run `resume --list` from another terminal → that session
     shows `ACTIVE`, all others show `READY`.
  2. Exit that OMP session. Re-run `resume --list` → **no** session shows `ACTIVE` (stale
     breadcrumb must not produce a false positive — the core of
     [R1](#r1-top--a-false-positive-active-makes---no-confirm-block-on-stdin)).
  3. With **no** `omp` running at all, confirm zero `ACTIVE` rows.
  4. Run `resume --no-confirm` against the active session → confirm the mandatory prompt appears
     (`launch.rs:161-163`). This is expected, documented behavior, not a bug.
- Update `mod.rs:47-56` and `docs/research/session-formats.md:127` with the now-known path.

### PR6 — dead-code cleanup

Apply the five dispositions in [section 9](#9-dead-code-dispositions): rename `ENV_SESSION_DIR`
→ `SESSION_DIR_FLAG` in `roots.rs`; delete `with_profile_flag`, `with_session_dir_flag`,
`was_live_growing`, `is_user_attributed_pub` in omp and `with_profile_flag`, `was_live_growing`
in `pi.rs`. Prune the `pub use` list accordingly.

```bash
rg 'ENV_SESSION_DIR|with_profile_flag|with_session_dir_flag|was_live_growing|is_user_attributed_pub' src tests
  # only omp's SESSION_DIR_FLAG definition and pi's REAL ENV_SESSION_DIR (pi.rs:54, :113) remain
cargo fmt --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo build --all-features --locked
cargo test --all-features --locked
```

- Net diff: 1 rename, 5 deletions. **If it compiles, nothing referenced them** — that is the
  whole verification.
- Independent of PR5; can land any time after PR4.

### PR7 — Pi symmetry (**deferred; named, not forgotten**)

Apply the same correlation to Pi: build a `SessionControlEvidence` map from `ctx.procs` and
replace the `None` at `src/app.rs:407`. `discover_pi`'s signature already accepts `ctx` from PR2,
so the diff is small.

```bash
cargo fmt --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --all-features --locked
```

- Requires the Pi equivalent of PR0. Pi's `SessionControlEvidence` (`pi.rs:563-601`, constructed
  only at `pi/tests.rs:1037`, `:1048`, `:1059`) is a *different* correlation — it matches on
  `session_id` + `transcript_path`, **not TTY** — so it needs a **session-control file reader**,
  a second undocumented on-disk format and a second probe task. That is why it is not wired in
  the same plan: it would double the review surface and double the unknowns, while the only
  genuinely shared piece (`ProcessTable`) is already agent-neutral.
- Fully optional; PR0–PR6 deliver the OMP feature standalone.

### Dependency graph

```
PR0 (probe) ─────────────────────────────────┐
                                             ▼
PR1 (proc.rs) ──► PR2 (context) ──► PR3 (wiring, inert) ──► PR5 (real breadcrumbs) ──► PR7 (pi)
                                             │
PR4 (pure-move split) ◄────────────────────┘  independent of PR3; rebase whichever lands second
     │
     ▼
PR6 (dead-code cleanup)   — strictly after PR4, independent of PR5
```

PR4 touches no line PR3 touches except the re-export surface, so the two can proceed in parallel
and rebase. If serialization is preferred, land PR4 first — reviewing a split against an
unchanged `app.rs` is easier than against a changed one. **PR6 (dead-code cleanup) must never be
folded into PR4**; see the governing rule in [section 9](#9-dead-code-dispositions).

---

## References

| Topic | Location |
|---|---|
| The break | `src/app.rs:531` (and `src/app.rs:407` for Pi) |
| Detection predicate | `src/integration/omp/mod.rs:833-880` |
| Existing activity tests | `src/integration/omp/tests.rs:1349-1441` |
| Confirm-prompt coupling | `src/launch.rs:143-147`, `src/launch.rs:160-163` |
| Status rendering | `src/app.rs:693-700` |
| Breadcrumb assertion without a path | `docs/research/session-formats.md:127`, `:131` |
| Named-profile isolation by omission | `src/integration/omp/mod.rs:298-303` |
| Resume argv order | `src/integration/omp/mod.rs:435-462` |
| Title resolution loop | `src/integration/omp/mod.rs:632-699` |
| Subprocess precedent | `src/scope.rs:174`, `:207`, `:221` |
| Raw-FFI precedent | `src/picker.rs:460-486` |
| Device-metadata precedent | `src/launch.rs:52-70` |
| Read-only invariant | `src/snapshot.rs:145`, `src/integration/omp/mod.rs:57` |
| Concurrency budgets | `src/runtime.rs:21-28` |
| Dependency policy | `deny.toml:12-21`, `:25`, `:29` |
| Verification commands | `CONTRIBUTING.md:19-21`, `.github/workflows/ci.yml:64-66` |
