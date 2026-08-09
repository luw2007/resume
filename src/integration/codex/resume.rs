use std::{ffi::OsString, path::Path};

use crate::session::{ResumeSpec, Session};

use super::{AGENT, ENV_CODEX_HOME};

/// Build the ResumeSpec for a Codex session.
///
/// Program is `codex`; argv is `-C <workspace> resume <uuid>`. The cwd is the
/// recorded Workspace. A nondefault `CODEX_HOME` is preserved as an
/// environment override so resume targets the same data root discovery used.
///
/// The override is derived from the session's provenance
/// (`key.effective_root`), not a fresh env read, so resume always targets the
/// same root that discovered the session — even if the env changes between
/// discovery and resume.
pub fn resume_spec(session: &Session, default_home: &Path) -> ResumeSpec {
    let workspace = session
        .workspace
        .workspace()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| default_home.to_path_buf());

    let argv = vec![
        OsString::from("-C"),
        workspace.clone().into_os_string(),
        OsString::from("resume"),
        session.resumable_id.clone(),
    ];

    let mut env = Vec::new();
    let discovered_root = &session.key.effective_root;
    if !discovered_root.as_os_str().is_empty() && !is_default_root(discovered_root, default_home) {
        env.push((
            OsString::from(ENV_CODEX_HOME),
            discovered_root.clone().into_os_string(),
        ));
    }

    ResumeSpec {
        program: OsString::from(AGENT),
        argv,
        cwd: workspace,
        env,
    }
}

/// True if `root` resolves to the default `~/.codex`-style root (i.e. no
/// explicit `CODEX_HOME` was applied). Used to decide whether to inject the
/// environment override on resume.
fn is_default_root(root: &Path, default_home: &Path) -> bool {
    let root_canon = root.canonicalize().ok();
    let default_canon = default_home.canonicalize().ok();
    match (root_canon, default_canon) {
        (Some(a), Some(b)) => a == b,
        _ => root == default_home,
    }
}
