use std::path::Path;

use super::ParsedSession;

/// Coarse outcome mirroring the feature-on [`sqlite::SqliteOutcome`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SqliteOutcome {
    /// SQLite enrichment is compiled out; the DB is never consulted.
    Absent,
    Used {
        enriched: usize,
        skipped_no_row: usize,
        diagnostics: Vec<crate::session::Diagnostic>,
    },
    Degraded {
        category: &'static str,
    },
}

impl SqliteOutcome {
    pub fn is_degraded(&self) -> bool {
        matches!(self, Self::Degraded { .. })
    }
    pub fn summary(&self) -> Option<String> {
        match self {
            Self::Absent => {
                Some("codex_sqlite_disabled: compiled without codex-sqlite feature".to_string())
            }
            _ => None,
        }
    }
}

/// No-op enrichment: with the feature off, sessions are returned unchanged.
pub fn enrich(_sessions: &mut [ParsedSession], _effective_root: &Path) -> SqliteOutcome {
    SqliteOutcome::Absent
}

/// Path the DB would occupy, for symmetry with the feature-on module.
pub fn state_db_path(effective_root: &Path) -> std::path::PathBuf {
    effective_root.join("state_5.sqlite")
}
