use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
};
/// An effective Claude root plus whether it came from a nondefault
/// `CLAUDE_CONFIG_DIR`.
///
/// `effective_root` is the resolved config root (`CLAUDE_CONFIG_DIR` or
/// `~/.claude`). It is part of Session provenance and identity: two roots with
/// identical transcripts are distinct Sessions. `nondefault` controls whether
/// the root is preserved on Resume as an environment override.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClaudeRoot {
    pub effective_root: PathBuf,
    pub nondefault: bool,
}

/// Resolve the effective Claude root from the environment and home directory.
///
/// Returns `None` when no root can be established (no `CLAUDE_CONFIG_DIR` and
/// no `HOME`). This function performs no filesystem mutation and never invokes
/// the Claude CLI.
pub fn resolve_root(config_dir_env: Option<&OsStr>, home: Option<&Path>) -> Option<ClaudeRoot> {
    if let Some(explicit) = config_dir_env
        && !explicit.is_empty()
    {
        return Some(ClaudeRoot {
            effective_root: PathBuf::from(explicit),
            nondefault: true,
        });
    }
    home.map(|home| ClaudeRoot {
        effective_root: home.join(".claude"),
        nondefault: false,
    })
}

#[cfg(test)]
#[path = "tests/roots.rs"]
mod tests;
