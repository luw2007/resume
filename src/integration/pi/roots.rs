use super::{
    DEFAULT_AGENT_ROOT_RELATIVE, DEFAULT_SESSIONS_DIR, ENV_AGENT_DIR, ENV_SESSION_DIR,
    SETTINGS_FILE, SETTINGS_SESSION_DIR_KEY,
};
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
};
/// Effective Pi roots resolved from environment and settings, without invoking
/// Pi. The agent root identifies where Pi stores its configuration; the session
/// root is where Session JSONL files live and may be custom (flat) or default
/// (grouped by encoded Workspace).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EffectiveRoots {
    /// The agent root (`<PI_CODING_AGENT_DIR>` or `~/.pi/agent`).
    pub agent_root: PathBuf,
    /// The effective session root: `PI_CODING_AGENT_SESSION_DIR`, settings
    /// `sessionDir`, or the default `<agent-root>/sessions`.
    pub session_root: PathBuf,
    /// Whether the session root is a custom (flat) root rather than the
    /// default grouped layout. Custom roots require header-`cwd` filtering
    /// because directory names are not encoded Workspaces.
    pub custom_session_root: bool,
}

/// Inputs to root resolution, abstracted for testability. Production code uses
/// [`ResolutionInputs::from_env`]; tests inject explicit values.
#[derive(Clone, Debug)]
pub struct ResolutionInputs {
    /// `$HOME`, used to resolve the default agent root.
    pub home: Option<PathBuf>,
    /// `PI_CODING_AGENT_DIR`, overriding the agent root.
    pub agent_dir_env: Option<PathBuf>,
    /// `PI_CODING_AGENT_SESSION_DIR`, overriding the session root.
    pub session_dir_env: Option<PathBuf>,
    /// Explicit `--session-dir` override (highest precedence for the session
    /// root). Discovery callers pass this through when present.
    pub session_dir_flag: Option<PathBuf>,
    /// Parsed `settings.json` content from the agent root, when present.
    pub settings: Option<Value>,
}

impl ResolutionInputs {
    /// Build resolution inputs from the process environment. Reads
    /// `PI_CODING_AGENT_DIR`, `PI_CODING_AGENT_SESSION_DIR`, and `$HOME`. The
    /// caller may attach parsed settings (see [`read_settings`]) before
    /// calling [`resolve`]. Never invokes Pi.
    pub fn from_env() -> Self {
        Self {
            home: std::env::var_os("HOME").map(PathBuf::from),
            agent_dir_env: std::env::var_os(ENV_AGENT_DIR).map(PathBuf::from),
            session_dir_env: std::env::var_os(ENV_SESSION_DIR).map(PathBuf::from),
            session_dir_flag: None,
            settings: None,
        }
    }

    /// Attach parsed settings before resolving.
    pub fn with_settings(mut self, settings: Option<Value>) -> Self {
        self.settings = settings;
        self
    }

    /// Attach an explicit `--session-dir` override before resolving.
    pub fn with_session_dir_flag(mut self, flag: Option<PathBuf>) -> Self {
        self.session_dir_flag = flag;
        self
    }
}

/// Resolve effective Pi roots. Precedence (session root): `--session-dir` flag,
/// then `PI_CODING_AGENT_SESSION_DIR`, then settings `sessionDir`, then the
/// default `<agent-root>/sessions`. Agent root precedence: `PI_CODING_AGENT_DIR`,
/// otherwise `~/.pi/agent`. Never invokes Pi.
///
/// Returns `None` when no agent root can be determined (no `PI_CODING_AGENT_DIR`
/// and no `$HOME`).
pub fn resolve(inputs: &ResolutionInputs) -> Option<EffectiveRoots> {
    let agent_root = agent_root(inputs)?;

    // Session root precedence.
    if let Some(flag) = &inputs.session_dir_flag {
        return Some(EffectiveRoots {
            agent_root,
            session_root: flag.clone(),
            custom_session_root: true,
        });
    }
    if let Some(env_root) = &inputs.session_dir_env {
        return Some(EffectiveRoots {
            agent_root,
            session_root: env_root.clone(),
            custom_session_root: true,
        });
    }
    if let Some(settings) = &inputs.settings
        && let Some(dir) = settings_dir(settings)
    {
        return Some(EffectiveRoots {
            agent_root,
            session_root: dir,
            custom_session_root: true,
        });
    }
    let session_root = agent_root.join(DEFAULT_SESSIONS_DIR);
    Some(EffectiveRoots {
        agent_root,
        session_root,
        custom_session_root: false,
    })
}

/// Resolve the agent root: `PI_CODING_AGENT_DIR` or `~/.pi/agent`.
fn agent_root(inputs: &ResolutionInputs) -> Option<PathBuf> {
    if let Some(env_root) = &inputs.agent_dir_env {
        return Some(env_root.clone());
    }
    inputs
        .home
        .as_deref()
        .map(|home| home.join(DEFAULT_AGENT_ROOT_RELATIVE))
}

/// Extract the `sessionDir` from parsed Pi settings JSON (string or object
/// `{ path }`).
pub(super) fn settings_dir(settings: &Value) -> Option<PathBuf> {
    match settings.get(SETTINGS_SESSION_DIR_KEY)? {
        Value::String(s) => Some(PathBuf::from(s)),
        Value::Object(obj) => obj.get("path").and_then(|v| v.as_str()).map(PathBuf::from),
        _ => None,
    }
}

/// Read and parse the Pi `settings.json` from an agent root. Returns `None`
/// when absent or unreadable; callers treat absence as "no settings override".
pub fn read_settings(agent_root: &Path) -> Option<Value> {
    let path = agent_root.join(SETTINGS_FILE);
    let text = fs::read_to_string(&path).ok()?;
    serde_json::from_str(&text).ok()
}

#[cfg(test)]
#[path = "tests/roots.rs"]
mod tests;
