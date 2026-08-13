//! OpenCode SQLite Agent Integration.
//!
//! Evidence: OpenCode 1.18.1.
//!
//! ## Storage
//!
//! - Effective root: `$XDG_DATA_HOME/opencode` if `XDG_DATA_HOME` is set and
//!   non-empty, otherwise `~/.local/share/opencode` (see [`roots`]).
//! - Sessions live exclusively in `opencode.db` (SQLite, `session` table).
//!   A `storage/session/*.json` / `storage/project/*.json` layout may exist
//!   from a pre-1.0 install; it is not written by current OpenCode and is
//!   never read by this integration — the database is authoritative.
//! - Identity: `session.id` (globally unique, stable, immutable). Workspace:
//!   `session.directory`, an absolute path recorded at session creation.
//!   OpenCode has no profile/isolation concept, so [`crate::session::SessionKey::profile`]
//!   is always `None`.
//!
//! ## Resume
//!
//! `opencode --session <id>` resumes the exact selected session in the
//! interactive TUI; the process runs with `cwd` set to `session.directory`
//! so OpenCode's own working-directory detection agrees with the recorded
//! Workspace. `--continue`/`-c` (last session) and `--fork` are deliberately
//! never used: they do not select *this* session, or they branch instead of
//! resuming it.
//!
//! ## Compiled without the `opencode` feature
//!
//! OpenCode has no non-SQLite session store, so without `rusqlite` linked
//! ([`--features opencode`][crate]) discovery returns no Sessions and one
//! `opencode_disabled` diagnostic rather than attempting a degraded scan.
//! This never blocks other agents' discovery.

pub const AGENT: &str = "opencode";

pub mod roots;

#[cfg(feature = "opencode")]
mod discover;
#[cfg(not(feature = "opencode"))]
#[path = "discover_stub.rs"]
mod discover;

mod resume;

pub use discover::{ParsedSession, discover};
pub use resume::{resume_spec, transcript_path};

#[cfg(test)]
mod tests;
