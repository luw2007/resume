//! Pi Agent integration.
//!
//! Evidence: Pi 0.84.1.
//! - Agent root: `PI_CODING_AGENT_DIR`, defaulting to `~/.pi/agent`.
//! - Default sessions root: `<agent-root>/sessions`, grouped by encoded
//!   resolved Workspace.
//! - Effective custom session root precedence: `--session-dir`,
//!   `PI_CODING_AGENT_SESSION_DIR`, settings `sessionDir`, then default.
//! - Append-only JSONL with a `type = "session"` header. Current version is 3;
//!   readers also understand v1 and v2. Header fields include stable `id`,
//!   `timestamp`, absolute `cwd`, and optional `parentSession`. A later
//!   `session_info.name` supplies the user display name.
//! - User entries contain `message.role = "user"`, string or typed block
//!   content, and message/entry timestamps.
//! - Pi may migrate old formats when opening them. `resume` parses read-only
//!   and never invokes Pi merely to inspect a Session.
//! - Safest exact Resume is `pi --session <absolute-jsonl-path>`, preserving
//!   `--session-dir <root>` when discovery used a custom root. Do not use
//!   `--session-id`.
//! - `~/.pi/session-control` sockets are positive activity evidence only when
//!   reliably tied to a Session; process presence alone is insufficient.
//!
//! This module never invokes Pi during discovery/preview. It reads JSONL
//! read-only through the shared [`crate::preview::jsonl`] reader, interprets records
//! using the shared [`crate::preview::message`], [`crate::preview::injection`], and
//! [`crate::preview::summary`] helpers, and produces [`crate::session::Session`] entries and
//! [`crate::session::ResumeSpec`]s.

mod discover;
mod format;
mod resume;
mod roots;

pub use discover::*;
pub use resume::*;
pub use roots::*;

/// Agent name used in Session keys and resume commands.
pub const AGENT: &str = "pi";
pub const ENV_AGENT_DIR: &str = "PI_CODING_AGENT_DIR";
pub const ENV_SESSION_DIR: &str = "PI_CODING_AGENT_SESSION_DIR";
pub const DEFAULT_AGENT_ROOT_RELATIVE: &str = ".pi/agent";
pub const DEFAULT_SESSIONS_DIR: &str = "sessions";
pub const SETTINGS_FILE: &str = "settings.json";
pub const SETTINGS_SESSION_DIR_KEY: &str = "sessionDir";
const DISCOVERY_SCAN_RECORDS: usize = 50_000;

#[cfg(test)]
#[path = "pi/test_support.rs"]
mod test_support;
