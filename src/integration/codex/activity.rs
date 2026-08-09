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
//!
//! ## Status
//!
//! Signatures only. See `docs/design/codex-active-detection-plan.md` for the
//! design of record; this module is not yet declared in `mod.rs` and is not
//! compiled until the module split lands.

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
