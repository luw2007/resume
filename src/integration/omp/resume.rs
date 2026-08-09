//! Exact native OMP Resume construction.

use super::{
    AGENT,
    format::ParsedSession,
    roots::{ENV_CONFIG_DIR, EffectiveRoots, ProfileSelection},
};
use crate::session::ResumeSpec;
use std::{ffi::OsString, path::PathBuf};

impl ParsedSession {
    pub fn resume_spec(&self, roots: &EffectiveRoots) -> ResumeSpec {
        let mut argv: Vec<OsString> = Vec::with_capacity(6);
        if let ProfileSelection::Named(name) = &roots.profile {
            argv.push(OsString::from("--profile"));
            argv.push(name.clone());
        }
        argv.push(OsString::from("--resume"));
        argv.push(OsString::from(self.id.clone()));
        if roots.custom_session_root {
            argv.push(OsString::from("--session-dir"));
            argv.push(roots.session_root.clone().into_os_string());
        }
        let cwd = self.workspace.clone().unwrap_or_else(|| PathBuf::from("."));

        // Preserve only an explicit root override. Injecting the default root
        // changes OMP's native resume lookup relative to direct invocation.
        let env = resume_env(roots);

        ResumeSpec {
            program: OsString::from(AGENT),
            argv,
            cwd,
            env,
        }
    }
}

/// Build the narrowly scoped environment overrides for an OMP resume.
/// All other resolution remains inherited from the child process environment.
fn resume_env(roots: &EffectiveRoots) -> Vec<(OsString, OsString)> {
    if roots.config_root_overridden {
        vec![(
            OsString::from(ENV_CONFIG_DIR),
            roots.config_root.clone().into_os_string(),
        )]
    } else {
        Vec::new()
    }
}
