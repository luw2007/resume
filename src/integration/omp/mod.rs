//! OMP Agent Integration (Step 7).
//!
//! OMP has the highest isolation risk of the four v0.1.0 integrations: a single
//! base config directory can host a default profile plus arbitrarily many named
//! profiles, each with its own agent root and optional XDG split roots, and
//! duplicate native IDs across those isolation boundaries must never collide.
//! Profile and effective root are therefore part of [`SessionKey`] identity.
//!
//! Evidence: OMP 17.2.10.
//!
//! ## Storage and profiles
//!
//! - Base: `PI_CONFIG_DIR`, defaulting to `~/.omp`.
//! - Default profile agent root: `<base>/agent`.
//! - Named profile agent root: `<base>/profiles/<name>/agent`.
//! - Profile selection precedence: `--profile` flag, then `OMP_PROFILE`,
//!   then `PI_PROFILE`.
//! - `PI_CODING_AGENT_DIR` overrides only the **unprofiled** agent root; named
//!   profiles deliberately ignore it.
//! - `--session-dir` overrides Session lookup for an invocation.
//! - Existing XDG OMP directories (`XDG_DATA_HOME`, `XDG_STATE_HOME`,
//!   `XDG_CACHE_HOME`) can split data/state/cache; root resolution mirrors the
//!   installed OMP behavior and is fixture-driven. Profile and effective root
//!   are part of Session provenance and identity. Workspace is never inferred
//!   from encoded or migrated directory names when the header is readable.
//!
//! ## Format
//!
//! JSONL normally begins with a padded title record (`type = "title"`, `v = 1`)
//! followed by a v3 `type = "session"` header with `id`, `timestamp`, absolute
//! `cwd`, and optional title metadata. Filenames are not authoritative. OMP's
//! title sidecar sits **before** the v3 header; we must not reuse Pi's
//! header-position assumptions (Pi's first session record is the header).
//!
//! User messages are typed envelopes with `message.role = "user"`, block
//! content, and attribution. Attribution is used to remove agent-injected
//! inputs. `title_change` records update title state.
//!
//! Imported Sessions receive a new OMP ID and a `foreign_session_import`
//! custom entry containing source kind, origin ID/path/cwd. Resume uses the
//! OMP header ID; only a safe origin badge is shown. The imported origin
//! Codex/Claude Session is never merged with this OMP Session.
//!
//! ## Resume and activity
//!
//! Default: `omp --resume <id>`. Named profile:
//! `omp --profile <name> --resume <id>`. `--session-dir <root>` is added when
//! discovery used it, and the process runs from the header `cwd`. An explicit
//! `PI_CONFIG_DIR` override is preserved through the environment.
//!
//! Terminal breadcrumbs map TTY names to cwd/session path but can be stale and
//! contain no PID. Active is reported only after correlating a live OMP
//! process, its TTY, and a matching breadcrumb Session path. A stale marker
//! alone is Unknown.
//!
//! This module never invokes OMP during discovery/preview. It reads JSONL
//! read-only through the shared [`crate::jsonl`] reader and interprets records
//! with the shared [`crate::message`], [`crate::injection`], and
//! [`crate::summary`] helpers.

use std::{
    ffi::OsString,
    fs, io,
    path::{Path, PathBuf},
    time::SystemTime,
};

use serde_json::Value;

use crate::{
    jsonl::{self, Bounds, FileOutcome, ReadResult},
    message::{self, UserMessage},
    scope::{self, Scope},
    session::{
        ActivityStatus, ResumeSpec, RiskStatus, Session, SessionKey, SupportStatus,
        WorkspaceEvidence,
    },
};

/// Agent name used in [`SessionKey::agent`].
pub const AGENT: &str = "omp";

/// Environment variable overriding the OMP base config directory.
pub const ENV_CONFIG_DIR: &str = "PI_CONFIG_DIR";
/// Environment variable overriding the **unprofiled** agent root only.
pub const ENV_AGENT_DIR: &str = "PI_CODING_AGENT_DIR";
/// Environment variable overriding the session root for an invocation.
pub const ENV_SESSION_DIR: &str = "--session-dir";
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

/// Maximum number of records to scan while extracting discovery metadata.
const DISCOVERY_SCAN_RECORDS: usize = 50_000;

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
    /// The profile name to embed in [`SessionKey::profile`], or `None` for the
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

    /// Attach an explicit `--profile` flag.
    pub fn with_profile_flag(mut self, flag: Option<OsString>) -> Self {
        self.profile_flag = flag;
        self
    }

    /// Attach an explicit `--session-dir` override.
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

/// Configuration for an OMP discovery pass.
#[derive(Clone, Debug)]
pub struct DiscoverConfig<'a> {
    /// Effective OMP roots for the selected profile.
    pub roots: EffectiveRoots,
    /// Scope used to filter Sessions by header `cwd`.
    pub scope: &'a Scope,
    /// Bounds for the JSONL reader. Discovery uses a record cap.
    pub bounds: Bounds,
}

impl<'a> DiscoverConfig<'a> {
    /// Discovery bounds with the default size limits and a record cap.
    pub fn new(roots: EffectiveRoots, scope: &'a Scope) -> Self {
        let bounds = Bounds {
            max_records: DISCOVERY_SCAN_RECORDS,
            ..Bounds::default()
        };
        Self {
            roots,
            scope,
            bounds,
        }
    }
}

/// A safe origin badge for an imported Session. The origin Codex/Claude Session
/// is never merged with the new OMP Session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImportBadge {
    /// Source agent kind (e.g. "codex", "claude").
    pub source_kind: String,
    /// Origin agent's native session ID, if recorded.
    pub origin_id: Option<String>,
    /// Origin Workspace, if recorded. Used only for the badge display.
    pub origin_cwd: Option<PathBuf>,
}

impl ImportBadge {
    /// A safe, short display string for the badge. Never exposes the origin
    /// repository remote or an absolute import source path verbatim.
    pub fn to_display(&self) -> String {
        let mut parts = vec![format!("imported from {}", self.source_kind)];
        if let Some(id) = &self.origin_id {
            // Show only a short prefix of the origin ID to avoid impersonating
            // it as a resumable locator.
            let short: String = id.chars().take(8).collect();
            parts.push(format!("origin:{short}"));
        }
        parts.join(" ")
    }
}

/// A discovered OMP session with its parsed metadata, ready to become a
/// [`Session`] via [`ParsedSession::into_session`].
#[derive(Clone, Debug)]
pub struct ParsedSession {
    /// Stable header `id` (the new OMP ID for imports).
    pub id: String,
    /// Authoritative header `cwd` (Workspace), when present.
    pub workspace: Option<PathBuf>,
    /// Header `timestamp` (epoch seconds).
    pub header_time: Option<SystemTime>,
    /// Resolved effective title across header/title/title_change state.
    pub title: Option<String>,
    /// Real user messages (terminal-safe, injection-filtered, attribution-aware).
    pub messages: Vec<UserMessage>,
    /// Canonical absolute transcript path (the locator used for dedupe).
    pub transcript_path: PathBuf,
    /// File mtime fallback for activity.
    pub file_mtime: Option<SystemTime>,
    /// Latest activity time from message/title timestamps, then header, then mtime.
    pub activity_time: Option<SystemTime>,
    /// Import badge, when this Session is an imported foreign Session.
    pub import: Option<ImportBadge>,
}

impl ParsedSession {
    /// Build a [`Session`] from this parsed data. Profile and effective root
    /// are embedded in the key so that duplicate IDs across profiles never
    /// collide.
    pub fn into_session(
        self,
        roots: &EffectiveRoots,
        risk: RiskStatus,
        activity: ActivityStatus,
    ) -> Session {
        let workspace_evidence = match &self.workspace {
            Some(workspace) => WorkspaceEvidence::Recorded {
                workspace: workspace.clone(),
                historical_git_identity: None,
            },
            None => WorkspaceEvidence::Unknown,
        };
        let title = match (self.title, self.import) {
            (Some(title), Some(import)) => Some(format!("{title} [{}]", import.to_display())),
            (None, Some(import)) => Some(import.to_display()),
            (title, None) => title,
        };
        Session {
            key: SessionKey {
                agent: OsString::from(AGENT),
                effective_root: roots.session_root.clone(),
                profile: roots.profile.as_profile_field(),
                native_locator: self.transcript_path.clone().into_os_string(),
            },
            resumable_id: OsString::from(self.id),
            title,
            workspace: workspace_evidence,
            support: SupportStatus::Supported,
            activity,
            risk,
        }
    }

    /// Build the [`ResumeSpec`]: default `omp --resume <id>`, or
    /// `omp --profile <name> --resume <id>` for a named profile. Adds
    /// `--session-dir <root>` when discovery used a custom root, sets the
    /// process cwd to the header Workspace, and preserves an explicit
    /// `PI_CONFIG_DIR` override in the environment. Never uses a shell.
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
/// Propagates an explicitly configured `PI_CONFIG_DIR` only. Default root
/// resolution must remain inherited from the child process environment.
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

/// Outcome of discovering OMP sessions in the effective session root.
#[derive(Clone, Debug, Default)]
pub struct DiscoverOutcome {
    /// Parsed sessions, before dedupe and Session construction.
    pub parsed: Vec<ParsedSession>,
    /// Number of JSONL files skipped due to read/parse errors (aggregated).
    pub skipped_files: usize,
    /// Number of files with no valid `session` header.
    pub no_header_files: usize,
    /// Number of files skipped because the header `cwd` was outside Scope.
    pub out_of_scope: usize,
}

/// Discover OMP sessions under the effective session root. Reads JSONL
/// read-only through the shared reader, parses the title sidecar + v3 header
/// (never assuming the header is the first record), and filters by header
/// `cwd` through Scope. Never invokes OMP or migrates files.
///
/// Discovery scans `.jsonl` files one level (or more) under the session root.
/// Header `cwd` is authoritative; directory names are never reversed.
pub fn discover(config: &DiscoverConfig<'_>) -> io::Result<DiscoverOutcome> {
    let session_root = config.roots.session_root.clone();
    let confined_root = session_root
        .canonicalize()
        .unwrap_or_else(|_| session_root.clone());
    let mut outcome = DiscoverOutcome::default();
    let mut seen: Vec<(PathBuf, PathBuf)> = Vec::new();

    for jsonl_path in iter_session_files(&session_root)? {
        let parsed = match parse_session_file(&jsonl_path, &confined_root, &config.bounds) {
            Ok(Some(parsed)) => parsed,
            Ok(None) => {
                outcome.no_header_files += 1;
                continue;
            }
            Err(_) => {
                outcome.skipped_files += 1;
                continue;
            }
        };

        // Dedupe: effective session root + canonical transcript locator.
        let canonical = jsonl_path
            .canonicalize()
            .unwrap_or_else(|_| jsonl_path.clone());
        let dedupe_key = (config.roots.session_root.clone(), canonical.clone());
        if seen.contains(&dedupe_key) {
            continue;
        }
        seen.push(dedupe_key);

        // Scope filtering via authoritative header cwd.
        match &parsed.workspace {
            Some(workspace) => {
                if !config.scope.contains_workspace(workspace) {
                    outcome.out_of_scope += 1;
                    continue;
                }
            }
            None => {
                // Missing Workspace: surfaced for diagnosis (Unavailable).
            }
        }

        outcome.parsed.push(parsed);
    }

    Ok(outcome)
}

/// Enumerate `.jsonl` files reachable from the session root. Tolerates a
/// missing session root (returns empty).
fn iter_session_files(session_root: &Path) -> io::Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    if !session_root.exists() {
        return Ok(paths);
    }
    collect_jsonl(session_root, &mut paths)?;
    paths.sort();
    Ok(paths)
}

/// Recursively collect `.jsonl` file paths over the storage layout (not Scope).
fn collect_jsonl(dir: &Path, out: &mut Vec<PathBuf>) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => continue,
        };
        let path = entry.path();
        let file_type = match entry.file_type() {
            Ok(ft) => ft,
            Err(_) => continue,
        };
        if file_type.is_dir() {
            collect_jsonl(&path, out)?;
        } else if (file_type.is_file() || file_type.is_symlink())
            && path.extension().and_then(|e| e.to_str()) == Some("jsonl")
        {
            out.push(path);
        }
    }
    Ok(())
}

/// Parse a single OMP JSONL session file read-only. Returns `Ok(None)` when
/// the file has no valid `session` header.
fn parse_session_file(
    path: &Path,
    effective_root: &Path,
    bounds: &Bounds,
) -> io::Result<Option<ParsedSession>> {
    let result = jsonl::read_file_confined(path, effective_root, bounds)?;
    let file_mtime = fs::metadata(path).and_then(|m| m.modified()).ok();
    Ok(extract_session(path, &result, file_mtime))
}

/// Title state accumulated from the title sidecar, the v3 header, and any
/// `title_change` records. Latest non-empty title wins.
#[derive(Clone, Debug, Default)]
struct TitleState {
    current: Option<String>,
}

impl TitleState {
    fn set(&mut self, title: Option<String>) {
        if let Some(t) = title
            && !t.trim().is_empty()
        {
            self.current = Some(t);
        }
    }
}

/// Extract a [`ParsedSession`] from a read result, or `None` if no valid
/// `session` header is present. Parses the title sidecar that may precede the
/// v3 header without assuming any record position.
fn extract_session(
    path: &Path,
    result: &ReadResult,
    file_mtime: Option<SystemTime>,
) -> Option<ParsedSession> {
    let header = find_session_header(&result.records)?;
    let id = header.get("id").and_then(|v| v.as_str())?.to_string();
    let workspace = header
        .get("cwd")
        .and_then(|v| v.as_str())
        .map(PathBuf::from);
    let header_time = header.get("timestamp").and_then(as_system_time);

    let mut title_state = TitleState::default();

    let mut messages = Vec::new();
    let mut latest_message_time: Option<SystemTime> = None;
    let mut import: Option<ImportBadge> = None;

    for record in &result.records {
        let rec_type = record.get("type").and_then(|v| v.as_str());

        // The v3 session header itself: extract its title metadata in record
        // order so that a title sidecar before it, the header, and a later
        // title_change all apply positionally (latest non-empty wins).
        if rec_type == Some("session") {
            title_state.set(
                record
                    .get("title")
                    .and_then(|v| v.as_str())
                    .map(String::from),
            );
            continue;
        }

        // Title sidecar: type == "title", v == 1 (padded). Applies before or
        // after the header positionally; we process in record order so a
        // later title_change can override it.
        if rec_type == Some("title") {
            title_state.set(
                record
                    .get("title")
                    .or_else(|| record.get("text"))
                    .and_then(|v| v.as_str())
                    .map(String::from),
            );
            continue;
        }

        // title_change records update title state.
        if rec_type == Some("title_change") {
            title_state.set(
                record
                    .get("title")
                    .and_then(|v| v.as_str())
                    .map(String::from),
            );
            continue;
        }

        // foreign_session_import custom entry → safe origin badge only.
        if rec_type == Some("custom") {
            if let Some(import_val) = record.get("foreign_session_import") {
                import = parse_import(import_val);
            }
            continue;
        }

        // User message records: message.role == "user", attribution-aware.
        if let Some(message_obj) = record.get("message").and_then(|v| v.as_object())
            && message_obj.get("role").and_then(|v| v.as_str()) == Some("user")
            && is_user_attributed(record, message_obj)
            && let Some(msg) = extract_user_message(message_obj)
        {
            let t = record
                .get("timestamp")
                .and_then(as_system_time)
                .or_else(|| message_obj.get("timestamp").and_then(as_system_time));
            if let Some(t) = t {
                latest_message_time = Some(match latest_message_time {
                    Some(current) if current >= t => current,
                    _ => t,
                });
            }
            messages.push(msg);
        }
    }

    let activity_time = latest_message_time.or(header_time).or(file_mtime);

    // Resolve effective title: accumulated title state, else summary from the
    // first valid human message.
    let title = title_state.current.or_else(|| {
        let texts: Vec<&str> = messages.iter().map(|m| m.text.as_str()).collect();
        crate::summary::summarize_texts(texts, crate::summary::default_width())
    });

    Some(ParsedSession {
        id,
        workspace,
        header_time,
        title,
        messages,
        transcript_path: path.to_path_buf(),
        file_mtime,
        activity_time,
        import,
    })
}

/// Find the first record with `type == "session"`. Unlike Pi, we make no
/// assumption that this is the first record: OMP may precede it with a padded
/// title sidecar.
fn find_session_header(records: &[Value]) -> Option<&Value> {
    records
        .iter()
        .find(|record| record.get("type").and_then(|v| v.as_str()) == Some("session"))
}

/// Determine whether a `message.role == "user"` record is genuinely
/// user-attributed, filtering out agent-injected inputs. OMP records may carry
/// an attribution field (e.g. `attribution.source`, `message.source`,
/// `meta.source`) marking injected/automated messages. Anything attributed to
/// the agent, the system, or a tool is excluded.
fn is_user_attributed(record: &Value, message_obj: &serde_json::Map<String, Value>) -> bool {
    // Check common attribution locations.
    let sources = [
        record.get("attribution").and_then(|v| v.get("source")),
        record.get("source"),
        message_obj.get("attribution").and_then(|v| v.get("source")),
        message_obj.get("source"),
        record.get("meta").and_then(|v| v.get("source")),
    ];
    for source in sources.into_iter().flatten() {
        if let Some(s) = source.as_str() {
            let lower = s.to_ascii_lowercase();
            // Reject agent/system/tool/injected/auto attributions.
            if lower == "agent"
                || lower == "assistant"
                || lower == "system"
                || lower == "tool"
                || lower == "injected"
                || lower == "auto"
                || lower == "automated"
            {
                return false;
            }
        }
        // A boolean "injected"/"automated" flag also excludes.
        if source == true {
            // Only treat bare `true` as exclusion if the key context implies it;
            // to be safe we look at the containing object key.
        }
    }
    // Explicit injected/automated boolean flags.
    let flags = [
        record.get("injected"),
        record.get("automated"),
        message_obj.get("injected"),
        message_obj.get("automated"),
        record.get("meta").and_then(|v| v.get("injected")),
    ];
    for flag in flags.into_iter().flatten() {
        if flag == true {
            return false;
        }
    }
    true
}

/// Extract a [`UserMessage`] from a `message` object with `role == "user"`.
fn extract_user_message(message_obj: &serde_json::Map<String, Value>) -> Option<UserMessage> {
    let content = message_obj
        .get("content")
        .or_else(|| message_obj.get("text"));
    let (text, attachments) = match content {
        Some(value) => message::extract_content(value),
        None => (None, Vec::new()),
    };
    if text.as_ref().is_some_and(|t| !t.trim().is_empty()) || !attachments.is_empty() {
        Some(message::build_user_message(text, attachments))
    } else {
        None
    }
}

/// Parse a `foreign_session_import` value into a safe [`ImportBadge`].
/// Origin repository remotes and absolute import source paths are not exposed.
fn parse_import(value: &Value) -> Option<ImportBadge> {
    let obj = value.as_object()?;
    let source_kind = obj
        .get("source_kind")
        .or_else(|| obj.get("kind"))
        .or_else(|| obj.get("source"))
        .and_then(|v| v.as_str())?
        .to_string();
    let origin_id = obj
        .get("origin_id")
        .or_else(|| obj.get("source_id"))
        .or_else(|| obj.get("original_id"))
        .and_then(|v| v.as_str())
        .map(String::from);
    let origin_cwd = obj
        .get("origin_cwd")
        .or_else(|| obj.get("source_cwd"))
        .or_else(|| obj.get("cwd"))
        .and_then(|v| v.as_str())
        .map(PathBuf::from);
    Some(ImportBadge {
        source_kind,
        origin_id,
        origin_cwd,
    })
}

/// Convert a JSON timestamp to `SystemTime`. See `crate::time::json_value_to_system_time`.
fn as_system_time(value: &Value) -> Option<SystemTime> {
    crate::time::json_value_to_system_time(value)
}

/// Determine the [`ActivityStatus`] for a parsed session. Active is reported
/// only when a live OMP process, its TTY, and a matching terminal breadcrumb
/// Session path all agree. A stale breadcrumb alone is Unknown; absence of
/// evidence is Unknown, never Inactive.
pub fn activity_status(
    parsed: &ParsedSession,
    evidence: Option<&ActivityEvidence>,
) -> ActivityStatus {
    match evidence {
        Some(evidence) if evidence.matches(parsed) => ActivityStatus::Active {
            observed_at: evidence.observed_at,
        },
        _ => ActivityStatus::Unknown,
    }
}

/// Positive-evidence correlation of a live OMP process, its TTY, and a
/// matching terminal breadcrumb. All three must agree for Active.
#[derive(Clone, Debug)]
pub struct ActivityEvidence {
    /// Whether a live OMP process was observed.
    pub live_process: bool,
    /// The TTY the live process is attached to.
    pub tty: Option<OsString>,
    /// Whether a terminal breadcrumb maps this TTY to a Session path.
    pub breadcrumb_alive: bool,
    /// Transcript path recorded in the breadcrumb.
    pub breadcrumb_session_path: PathBuf,
    /// When the correlation was observed.
    pub observed_at: SystemTime,
}

impl ActivityEvidence {
    /// A match requires a live process, a TTY, an alive breadcrumb, and the
    /// breadcrumb Session path resolving to the parsed transcript locator.
    fn matches(&self, parsed: &ParsedSession) -> bool {
        if !self.live_process || self.tty.is_none() || !self.breadcrumb_alive {
            return false;
        }
        let self_canon = self.breadcrumb_session_path.canonicalize().ok();
        let parsed_canon = parsed.transcript_path.canonicalize().ok();
        match (self_canon, parsed_canon) {
            (Some(a), Some(b)) => a == b,
            _ => self.breadcrumb_session_path == parsed.transcript_path,
        }
    }
}

/// Compute risk status for a parsed OMP session, including the broad-workspace
/// check against `$HOME`/`/`.
pub fn risk_status(parsed: &ParsedSession, home: Option<&Path>) -> RiskStatus {
    let evidence = match &parsed.workspace {
        Some(workspace) => WorkspaceEvidence::Recorded {
            workspace: workspace.clone(),
            historical_git_identity: None,
        },
        None => return RiskStatus::Normal,
    };
    scope::broad_workspace_risk(&evidence, home)
}

/// Whether a read result indicates a file that was being actively written.
pub fn was_live_growing(result: &ReadResult) -> bool {
    matches!(result.outcome, FileOutcome::IncompleteTail)
}

// ---------------------------------------------------------------------------
// Test-exposed wrappers.
// ---------------------------------------------------------------------------

#[doc(hidden)]
pub fn extract_session_pub(
    path: &Path,
    result: &ReadResult,
    file_mtime: Option<SystemTime>,
) -> Option<ParsedSession> {
    extract_session(path, result, file_mtime)
}

#[doc(hidden)]
pub fn parse_import_pub(value: &Value) -> Option<ImportBadge> {
    parse_import(value)
}

#[doc(hidden)]
pub fn is_user_attributed_pub(
    record: &Value,
    message_obj: &serde_json::Map<String, Value>,
) -> bool {
    is_user_attributed(record, message_obj)
}

#[cfg(test)]
mod tests;
