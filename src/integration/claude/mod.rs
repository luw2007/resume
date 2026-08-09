//! Claude Code Agent Integration.
//!
//! Evidence: Claude Code 2.1.220, root `CLAUDE_CONFIG_DIR` (default `~/.claude`),
//! heterogeneous top-level JSONL transcripts under `projects/<workspace-key>/`.
//! The workspace key replaces non-alphanumeric characters and is collision-prone;
//! it is **never** reversed to infer Workspace. Per-record event `cwd` is the
//! authoritative Workspace source.
//!
//! ## Identity contract
//!
//! Accept a stable identity only when the UUID filename and the embedded
//! `sessionId` agree. Per-record `uuid` is not the resumable Session ID. When
//! filename and embedded ID disagree (or the embedded ID is absent), the
//! Session is diagnosed/skipped or marked Discover Only according to the
//! evidence available — it is never silently resumed under a guessed ID.
//!
//! ## Title precedence
//!
//! Explicit agent/display name, then AI title, then a deterministic summary of
//! the first valid human text input. This precedence is **Resume presentation**,
//! not a claim of native picker equivalence — the native Claude title precedence
//! was not safely invoked in research.
//!
//! ## Resume contract
//!
//! `claude --resume <uuid>` with the authoritative Workspace as the child cwd,
//! preserving a nondefault `CLAUDE_CONFIG_DIR`. Never `--continue` for exact
//! Resume.
//!
//! ## Activity
//!
//! No authoritative active marker was found. Activity is [`ActivityStatus::Unknown`]
//! absent a future validated positive process/session association.

mod discover;
mod format;
mod resume;
mod roots;

pub use discover::*;
pub use resume::*;
pub use roots::*;

pub const AGENT: &str = "claude";
pub const CONFIG_DIR_ENV: &str = "CLAUDE_CONFIG_DIR";

#[cfg(test)]
#[path = "test_support.rs"]
mod test_support;
