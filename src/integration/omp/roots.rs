//! OMP profile and storage-root resolution.

use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

/// Environment variable overriding the OMP base config directory.
pub const ENV_CONFIG_DIR: &str = "PI_CONFIG_DIR";
/// Environment variable overriding the **unprofiled** agent root only.
pub const ENV_AGENT_DIR: &str = "PI_CODING_AGENT_DIR";
/// Environment variable overriding the session root for an invocation.
pub const FLAG_SESSION_DIR: &str = "--session-dir";
/// Environment variable selecting a profile (lower precedence than `OMP_PROFILE`).
pub const ENV_PI_PROFILE: &str = "PI_PROFILE";
/// Environment variable selecting a profile (higher precedence than `PI_PROFILE`).
pub const ENV_OMP_PROFILE: &str = "OMP_PROFILE";

/// Default base config directory relative to `$HOME`.
pub const DEFAULT_BASE_RELATIVE: &str = ".omp";
/// Default agent root directory name under the base.
pub const AGENT_DIR_NAME: &str = "agent";
/// Profiles directory name under the base.
pub const PROFILES_DIR_NAME: &str = "profiles";
/// Directory inside a named profile holding its agent root.
pub const PROFILE_AGENT_DIR_NAME: &str = "agent";
/// `XDG_DATA_HOME` environment variable. When set under OMP it overrides the
/// data root (where `agent/` lives) for the default profile.
pub const ENV_XDG_DATA_HOME: &str = "XDG_DATA_HOME";

/// A named profile selection, abstracted so callers can pass the CLI flag,
/// env vars, or a test value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProfileSelection {
    /// Default (unprofiled) agent root.
    Default,
    /// A named profile, e.g. `--profile work` or `OMP_PROFILE=work`.
    Named(OsString),
}

impl ProfileSelection {
    /// The profile name to embed in [`crate::session::SessionKey::profile`], or `None` for the
    /// default profile.
    pub fn as_profile_field(&self) -> Option<OsString> {
        match self {
            Self::Default => None,
            Self::Named(name) => Some(name.clone()),
        }
    }
}

/// Effective OMP roots for a profile, resolved from environment without
/// invoking OMP.
///
/// The `agent_root` is where OMP stores Sessions for the selected profile; the
/// `session_root` is where Session JSONL files live and may be custom. The
/// `config_root` and its provenance determine whether `PI_CONFIG_DIR` must be
/// preserved in the resume environment.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EffectiveRoots {
    /// Base config root (`PI_CONFIG_DIR` or `~/.omp`).
    pub config_root: PathBuf,
    /// Whether `config_root` was selected by an explicit `PI_CONFIG_DIR`.
    pub config_root_overridden: bool,
    /// Agent root for the selected profile (`<base>/agent` or
    /// `<base>/profiles/<name>/agent`).
    pub agent_root: PathBuf,
    /// Effective session root: `--session-dir`, otherwise the agent root.
    pub session_root: PathBuf,
    /// Whether the session root is a custom override (`--session-dir`).
    pub custom_session_root: bool,
    /// The selected profile.
    pub profile: ProfileSelection,
}

/// Inputs to root resolution, abstracted for testability. Production code uses
/// [`ResolutionInputs::from_env`]; tests inject explicit values.
#[derive(Clone, Debug, Default)]
pub struct ResolutionInputs {
    /// `$HOME`, used to resolve the default base config root.
    pub home: Option<PathBuf>,
    /// `PI_CONFIG_DIR`, overriding the base config root.
    pub config_dir_env: Option<PathBuf>,
    /// `PI_CODING_AGENT_DIR`, overriding only the unprofiled agent root.
    pub agent_dir_env: Option<PathBuf>,
    /// `--session-dir` override (highest precedence for the session root).
    pub session_dir_flag: Option<PathBuf>,
    /// `XDG_DATA_HOME`, overriding the default-profile data root.
    pub xdg_data_home: Option<PathBuf>,
    /// Explicit `--profile` flag (highest precedence for profile selection).
    pub profile_flag: Option<OsString>,
    /// `OMP_PROFILE` environment variable.
    pub omp_profile_env: Option<OsString>,
    /// `PI_PROFILE` environment variable.
    pub pi_profile_env: Option<OsString>,
}

impl ResolutionInputs {
    /// Build resolution inputs from the process environment. Reads
    /// `PI_CONFIG_DIR`, `PI_CODING_AGENT_DIR`, `XDG_DATA_HOME`, `OMP_PROFILE`,
    /// `PI_PROFILE`, and `$HOME`. The caller attaches the `--session-dir` and
    /// `--profile` CLI flags via the builder methods. Never invokes OMP.
    pub fn from_env() -> Self {
        Self {
            home: std::env::var_os("HOME").map(PathBuf::from),
            config_dir_env: std::env::var_os(ENV_CONFIG_DIR).map(PathBuf::from),
            agent_dir_env: std::env::var_os(ENV_AGENT_DIR).map(PathBuf::from),
            session_dir_flag: None,
            xdg_data_home: std::env::var_os(ENV_XDG_DATA_HOME).map(PathBuf::from),
            profile_flag: None,
            omp_profile_env: std::env::var_os(ENV_OMP_PROFILE),
            pi_profile_env: std::env::var_os(ENV_PI_PROFILE),
        }
    }

    /// Attach an explicit `--session-dir` override.
    // Retained for the future public `--session-dir` option.
    #[allow(dead_code)]
    pub fn with_session_dir_flag(mut self, flag: Option<PathBuf>) -> Self {
        self.session_dir_flag = flag;
        self
    }
}

/// Resolve the selected profile from inputs. Precedence: `--profile` flag,
/// then `OMP_PROFILE`, then `PI_PROFILE`. A whitespace-only name is ignored.
pub fn select_profile(inputs: &ResolutionInputs) -> ProfileSelection {
    if let Some(name) = &inputs.profile_flag
        && is_nonempty_profile(name)
    {
        return ProfileSelection::Named(name.clone());
    }
    if let Some(name) = &inputs.omp_profile_env
        && is_nonempty_profile(name)
    {
        return ProfileSelection::Named(name.clone());
    }
    if let Some(name) = &inputs.pi_profile_env
        && is_nonempty_profile(name)
    {
        return ProfileSelection::Named(name.clone());
    }
    ProfileSelection::Default
}

fn is_nonempty_profile(name: &OsString) -> bool {
    name.to_string_lossy().trim().is_empty().not()
}

/// Local trait to avoid importing a bool-extending crate.
trait BoolNot {
    fn not(self) -> bool;
}

impl BoolNot for bool {
    fn not(self) -> bool {
        !self
    }
}

/// Resolve effective OMP roots for the selected profile.
///
/// Agent root precedence:
/// - Default profile: `PI_CODING_AGENT_DIR`, then the default data root
///   (`XDG_DATA_HOME` if set, otherwise `<config-root>/agent`).
/// - Named profile: always `<config-root>/profiles/<name>/agent`; named
///   profiles deliberately ignore `PI_CODING_AGENT_DIR`.
///
/// Session root precedence: `--session-dir` flag, otherwise the agent root.
///
/// Config root precedence: `PI_CONFIG_DIR`, otherwise `~/.omp`. Returns
/// `None` when no config root can be determined.
pub fn resolve(inputs: &ResolutionInputs) -> Option<EffectiveRoots> {
    let config_root = config_root(inputs)?;
    let profile = select_profile(inputs);
    let agent_root = agent_root(&config_root, &profile, inputs);
    let (session_root, custom_session_root) = session_root(&agent_root, inputs);
    Some(EffectiveRoots {
        config_root,
        config_root_overridden: inputs.config_dir_env.is_some(),
        agent_root,
        session_root,
        custom_session_root,
        profile,
    })
}

/// Resolve the base config root: `PI_CONFIG_DIR` or `~/.omp`.
fn config_root(inputs: &ResolutionInputs) -> Option<PathBuf> {
    if let Some(env_root) = &inputs.config_dir_env {
        return Some(env_root.clone());
    }
    inputs
        .home
        .as_deref()
        .map(|home| home.join(DEFAULT_BASE_RELATIVE))
}

/// Resolve the agent root for the selected profile.
fn agent_root(
    config_root: &Path,
    profile: &ProfileSelection,
    inputs: &ResolutionInputs,
) -> PathBuf {
    match profile {
        ProfileSelection::Default => {
            // PI_CODING_AGENT_DIR overrides only the unprofiled agent root.
            if let Some(env_root) = &inputs.agent_dir_env {
                return env_root.clone();
            }
            // XDG_DATA_HOME overrides the default-profile data root.
            if let Some(xdg) = &inputs.xdg_data_home {
                return xdg.join(AGENT_DIR_NAME);
            }
            config_root.join(AGENT_DIR_NAME)
        }
        // Named profiles deliberately ignore PI_CODING_AGENT_DIR and XDG_DATA_HOME.
        // DO NOT unify these branches — named-profile isolation is by omission.
        ProfileSelection::Named(name) => config_root
            .join(PROFILES_DIR_NAME)
            .join(name)
            .join(PROFILE_AGENT_DIR_NAME),
    }
}

/// Resolve the session root. Custom (`--session-dir`) overrides the agent root.
fn session_root(agent_root: &Path, inputs: &ResolutionInputs) -> (PathBuf, bool) {
    if let Some(flag) = &inputs.session_dir_flag {
        return (flag.clone(), true);
    }
    (agent_root.to_path_buf(), false)
}
