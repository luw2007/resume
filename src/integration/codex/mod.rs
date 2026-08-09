//! Codex JSONL Agent Integration (Step 6).
//!
//! Discovers Codex Sessions from rollout JSONL under the effective `CODEX_HOME`
//! (defaulting to `~/.codex`), without depending on SQLite (`state_5.sqlite`)
//! or legacy indexes (`session_index.jsonl`, `history.jsonl`). The rollout JSONL
//! is authoritative for identity and Workspace.
//!
//! ## Identity and Workspace
//!
//! The stable identity is `session_meta.payload.id`, **not** the filename and
//! **not** the unrelated `payload.session_id`. The Workspace is
//! `session_meta.payload.cwd`, never the storage directory or
//! `workspace_roots`.
//!
//! ## User message extraction
//!
//! User input may appear twice in a rollout: as an `event_msg` record with
//! `payload.type = "user_message"`, and as a `response_item` record whose
//! message has `role = "user"`. The two representations are deduplicated.
//! Developer/system injections and environmental context are excluded.
//!
//! ## Source/import badges
//!
//! `source`, `thread_source`, `parent_thread_id`, and import metadata are
//! preserved only as safe badges. The repository remote and import source path
//! are never displayed by default.
//!
//! ## Resume
//!
//! [`codex -C <workspace> resume <uuid>`][resume], preserving a nondefault
//! `CODEX_HOME` via the environment. Codex 0.146.0 does not document Resume by
//! rollout path, so the rollout path is never used as the resume locator.
//!
//! ## Activity
//!
//! Implemented positive-evidence only: one `lsof` probe per discovery run indexes
//! rollout files held open by live Codex processes. An exact path or confirmed
//! device/inode match yields Active; absence remains
//! [`ActivityStatus::Unknown`][crate::session::ActivityStatus::Unknown], never Inactive.
//! Resuming an Active session prompts even under `--no-confirm`, because risk
//! prompts cannot be bypassed. This does not depend on SQLite enrichment.
//!
//! [resume]: crate::session::ResumeSpec

pub const AGENT: &str = "codex";

pub mod activity;
pub(crate) mod discover;
pub(crate) mod resume;
pub(crate) mod roots;

pub use discover::{
    DiscoveredSession, ImportMeta, ParsedSession, build_session, discover, discover_with_filter,
    discover_with_filter_enriched, extract_user_messages, parse_rollout_file,
};
pub use resume::resume_spec;
pub use roots::{ENV_CODEX_HOME, RolloutKind, RolloutRoot, effective_root, rollout_roots};

#[cfg(feature = "codex-sqlite")]
pub mod sqlite;
#[cfg(not(feature = "codex-sqlite"))]
#[path = "sqlite_stub.rs"]
pub mod sqlite;

#[cfg(test)]
mod tests;
