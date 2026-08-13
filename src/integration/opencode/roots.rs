use std::path::{Path, PathBuf};

/// Environment variable naming the effective XDG data-home base. When set and
/// non-empty, OpenCode's data root is `$XDG_DATA_HOME/opencode`.
pub const ENV_XDG_DATA_HOME: &str = "XDG_DATA_HOME";

/// Default data root relative to `$HOME` when `XDG_DATA_HOME` is unset.
pub const DEFAULT_BASE_RELATIVE: &str = ".local/share/opencode";

/// Filename of the OpenCode SQLite database beneath the effective root.
///
/// Evidence: OpenCode 1.18.1 persists sessions exclusively in this database
/// (`session` table). An older `storage/session/*.json` / `storage/project/*.json`
/// layout exists on disk from a pre-1.0 install but is not written by current
/// OpenCode and is not read by this integration — the database is
/// authoritative.
pub const DB_FILENAME: &str = "opencode.db";

/// Resolve the effective OpenCode data root.
///
/// Precedence: `$XDG_DATA_HOME/opencode` if `XDG_DATA_HOME` is set and
/// non-empty, otherwise `~/.local/share/opencode`. The returned path is not
/// canonicalized here (the root may not exist yet); callers canonicalize
/// when building identity.
pub fn effective_root() -> Option<PathBuf> {
    match std::env::var_os(ENV_XDG_DATA_HOME) {
        Some(value) if !value.is_empty() => Some(PathBuf::from(value).join("opencode")),
        _ => dirs_home().map(|home| home.join(DEFAULT_BASE_RELATIVE)),
    }
}

/// Resolve `$HOME` without depending on a `dirs` crate.
fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|home| !home.as_os_str().is_empty())
}

/// Path to the session database beneath an effective root.
pub fn db_path(effective_root: &Path) -> PathBuf {
    effective_root.join(DB_FILENAME)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xdg_data_home_overrides_default() {
        // SAFETY: test-local, single-threaded env mutation confined to this function.
        unsafe {
            std::env::set_var(ENV_XDG_DATA_HOME, "/xdg/data");
        }
        assert_eq!(effective_root(), Some(PathBuf::from("/xdg/data/opencode")));
        unsafe {
            std::env::remove_var(ENV_XDG_DATA_HOME);
        }
    }

    #[test]
    fn db_path_joins_filename() {
        assert_eq!(
            db_path(Path::new("/root")),
            PathBuf::from("/root/opencode.db")
        );
    }
}
