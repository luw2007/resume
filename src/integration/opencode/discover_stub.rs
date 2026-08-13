//! No-op discovery compiled when the `opencode` cargo feature is off.
//!
//! OpenCode has no non-SQLite session store, so without `rusqlite` linked
//! there is nothing this module can read. It returns `Ok(None)`, which the
//! `resume` binary surfaces as `opencode_disabled` — the same shape as a
//! missing database, never a hard failure that would block other agents.

use std::path::Path;
use std::time::SystemTime;

use crate::session::Session;

use super::AGENT;

/// Mirrors the feature-on [`ParsedSession`](super::ParsedSession) shape so
/// callers compile unchanged with the feature off.
#[derive(Clone, Debug)]
pub struct ParsedSession {
    pub id: String,
    pub directory: std::path::PathBuf,
    pub title: Option<String>,
    pub updated_at: Option<SystemTime>,
}

impl ParsedSession {
    pub fn into_session(self, _effective_root: &Path, _home: Option<&Path>) -> Session {
        unreachable!("opencode feature is off: no ParsedSession is ever produced")
    }
}

#[derive(Clone, Debug, Default)]
pub struct DiscoverOutcome {
    pub parsed: Vec<ParsedSession>,
    pub skipped_rows: usize,
}

/// Always reports "no database" without touching the filesystem, since no
/// SQLite reader is linked in this build.
pub fn discover(_effective_root: &Path) -> Result<Option<DiscoverOutcome>, DiscoverDisabled> {
    let _ = AGENT;
    Ok(None)
}

#[derive(Debug)]
pub struct DiscoverDisabled;
