//! Top-level discovery, picker/list output, revalidation, confirmation, and exec orchestration.

use std::{
    collections::HashMap,
    io,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    thread,
};

use serde::Serialize;

use crate::{
    cli::Cli,
    config::{Config, PreviewMode, PreviewPosition},
    diagnostics::{redact_path, redact_text},
    integration::{claude, codex, omp, pi},
    jsonl::Bounds,
    launch::{self, LaunchEvidence},
    picker::{CandidateKey, PickerCandidate, PickerOutcome},
    runtime::{CancelToken, JOIN_BUDGET, join_with_budget},
    scope::{DefaultScope, Direction, Scope, WorkspaceCandidate},
    session::{ActivityStatus, Diagnostic, ResumeSpec, Session, SupportStatus},
    text,
};

pub const EXIT_OK: i32 = 0;
pub const EXIT_ERROR: i32 = 1;
pub const EXIT_USAGE: i32 = 2;
pub const EXIT_INTERRUPT: i32 = 130;

#[derive(Clone, Debug)]
struct CandidateRecord {
    session: Session,
    spec: Option<ResumeSpec>,
    evidence: Option<LaunchEvidence>,
}

#[derive(Default)]
struct DiscoveryState {
    sessions: AtomicUsize,
    successful_integrations: AtomicUsize,
    errors: Mutex<Vec<Diagnostic>>,
}

#[derive(Clone)]
struct EffectiveOptions {
    agents: Vec<String>,
    confirm_always: bool,
    no_confirm: bool,
    preview: PreviewMode,
    preview_position: PreviewPosition,
    verbose: bool,
}

pub fn run(cli: Cli) -> i32 {
    let (config, _) = match crate::config::load(cli.config.clone()) {
        Ok(value) => value,
        Err(error) => {
            eprintln!("resume: {error}");
            return EXIT_USAGE;
        }
    };
    let options = match effective_options(&cli, config) {
        Ok(options) => options,
        Err(error) => {
            eprintln!("resume: {error}");
            return EXIT_USAGE;
        }
    };
    let scope = match build_scope(&cli) {
        Ok(scope) => Arc::new(scope),
        Err(error) => {
            eprintln!("resume: {error}");
            return EXIT_USAGE;
        }
    };

    if cli.list || cli.json {
        let (records, state) = discover_all(&options, scope);
        print_diagnostics(&state, options.verbose);
        if cli.json {
            print_json(&records, &state);
        } else {
            print_list(&records);
        }
        return discovery_exit(&records, &state);
    }

    run_interactive(&options, scope)
}

fn effective_options(cli: &Cli, config: Config) -> Result<EffectiveOptions, String> {
    let agents: Vec<String> = if !cli.agent.is_empty() {
        cli.agent
            .iter()
            .map(|a| a.to_string_lossy().to_ascii_lowercase())
            .collect()
    } else {
        config
            .agents
            .unwrap_or_else(|| vec!["codex".into(), "claude".into(), "pi".into(), "omp".into()])
    };
    for agent in &agents {
        if !matches!(agent.as_str(), "pi" | "claude" | "codex" | "omp") {
            return Err(format!("unknown agent {agent:?}"));
        }
    }
    Ok(EffectiveOptions {
        agents,
        confirm_always: cli.confirm_always || config.confirm_always.unwrap_or(false),
        no_confirm: cli.no_confirm,
        preview: config.preview.unwrap_or(PreviewMode::Hidden),
        preview_position: config.preview_position.unwrap_or(PreviewPosition::Auto),
        verbose: cli.verbose || config.verbose.unwrap_or(false),
    })
}

fn build_scope(cli: &Cli) -> io::Result<Scope> {
    let base = crate::scope::canonical_base(cli.directory.as_deref().unwrap_or(Path::new(".")))?;
    let direction = cli
        .up
        .clone()
        .map(Direction::Up)
        .or_else(|| cli.down.clone().map(Direction::Down));
    let default = if direction.is_none() {
        match crate::scope::discover_git_scope(&base) {
            Ok(git) => DefaultScope::Git {
                common_dir: git.common_dir,
                worktrees: git.worktrees,
            },
            Err(error) => DefaultScope::Exact {
                git_warning: Some(error.to_string()),
            },
        }
    } else {
        DefaultScope::Exact { git_warning: None }
    };
    Ok(Scope::new(base, direction, default))
}

fn discover_all(
    options: &EffectiveOptions,
    scope: Arc<Scope>,
) -> (Vec<CandidateRecord>, Arc<DiscoveryState>) {
    let state = Arc::new(DiscoveryState::default());
    let records = Arc::new(Mutex::new(Vec::new()));
    let cancel = CancelToken::new();
    let mut handles = Vec::new();
    for agent in &options.agents {
        let agent = agent.clone();
        let scope = scope.clone();
        let state = state.clone();
        let records = records.clone();
        let cancel = cancel.clone();
        handles.push(thread::spawn(move || {
            let result = discover_agent(&agent, &scope, &cancel);
            if result.integration_ok {
                state.successful_integrations.fetch_add(1, Ordering::SeqCst);
            }
            state
                .sessions
                .fetch_add(result.records.len(), Ordering::SeqCst);
            state.errors.lock().unwrap().extend(result.errors);
            records.lock().unwrap().extend(result.records);
        }));
    }
    for handle in handles {
        let _ = handle.join();
    }
    let mut result = Arc::try_unwrap(records).ok().unwrap().into_inner().unwrap();
    result.sort_by(|a, b| crate::session::compare_sessions(&a.session, &b.session));
    (result, state)
}

fn run_interactive(options: &EffectiveOptions, scope: Arc<Scope>) -> i32 {
    let state = Arc::new(DiscoveryState::default());
    let map: Arc<Mutex<HashMap<CandidateKey, CandidateRecord>>> =
        Arc::new(Mutex::new(HashMap::new()));
    let next_key = Arc::new(AtomicU64::new(1));
    let cancel = CancelToken::new();
    let (tx, rx) = std::sync::mpsc::sync_channel(crate::runtime::CHANNEL_CAPACITY);
    let mut handles = Vec::new();
    for agent in &options.agents {
        let agent = agent.clone();
        let scope = scope.clone();
        let state = state.clone();
        let map = map.clone();
        let next_key = next_key.clone();
        let cancel = cancel.clone();
        let tx = tx.clone();
        handles.push(thread::spawn(move || {
            let result = discover_agent(&agent, &scope, &cancel);
            if result.integration_ok {
                state.successful_integrations.fetch_add(1, Ordering::SeqCst);
            }
            state.errors.lock().unwrap().extend(result.errors);
            for record in result.records {
                if cancel.is_cancelled() {
                    break;
                }
                let key = CandidateKey(next_key.fetch_add(1, Ordering::SeqCst));
                let item = picker_candidate(key.clone(), &record.session);
                map.lock().unwrap().insert(key, record);
                state.sessions.fetch_add(1, Ordering::SeqCst);
                if tx.send(item).is_err() {
                    break;
                }
            }
        }));
    }
    drop(tx);
    let outcome =
        crate::picker::run_production_picker(rx, options.preview, options.preview_position);
    cancel.cancel();
    let _ = join_with_budget(handles, JOIN_BUDGET);
    print_diagnostics(&state, options.verbose);
    match outcome {
        PickerOutcome::Cancelled => {
            if state.sessions.load(Ordering::SeqCst) == 0
                && state.successful_integrations.load(Ordering::SeqCst) == 0
            {
                EXIT_ERROR
            } else {
                EXIT_OK
            }
        }
        PickerOutcome::Interrupted => EXIT_INTERRUPT,
        PickerOutcome::PreflightFailed(reason) | PickerOutcome::InternalError(reason) => {
            eprintln!("resume: {reason}");
            EXIT_ERROR
        }
        PickerOutcome::Selected(key) => {
            let Some(record) = map.lock().unwrap().get(&key).cloned() else {
                eprintln!("resume: selected Session disappeared");
                return EXIT_ERROR;
            };
            resume_selected(record, options)
        }
    }
}

fn resume_selected(record: CandidateRecord, options: &EffectiveOptions) -> i32 {
    if record.session.support != SupportStatus::Supported {
        eprintln!(
            "resume: selected Session is unavailable: {:?}",
            record.session.support
        );
        return EXIT_USAGE;
    }
    let (Some(spec), Some(evidence)) = (record.spec.as_ref(), record.evidence.as_ref()) else {
        eprintln!("resume: selected Session cannot be resumed");
        return EXIT_USAGE;
    };
    if let Err(error) = launch::revalidate(&record.session, spec, evidence) {
        eprintln!("resume: {error}");
        return EXIT_ERROR;
    }
    let reasons = launch::risk_reasons(&record.session, options.confirm_always);
    if launch::should_confirm(&record.session, options.confirm_always, options.no_confirm) {
        let stdin = io::stdin();
        let mut input = stdin.lock();
        let mut stderr = io::stderr();
        match launch::confirm(&mut input, &mut stderr, &record.session, &reasons) {
            Ok(true) => {}
            Ok(false) => return EXIT_OK,
            Err(error) => {
                eprintln!("resume: confirmation failed: {error}");
                return EXIT_ERROR;
            }
        }
    }
    let error = launch::exec(spec);
    eprintln!("resume: unable to launch {:?}: {error}", spec.program);
    EXIT_ERROR
}

struct AgentDiscovery {
    records: Vec<CandidateRecord>,
    errors: Vec<Diagnostic>,
    integration_ok: bool,
}
impl AgentDiscovery {
    fn ok(records: Vec<CandidateRecord>, errors: Vec<Diagnostic>) -> Self {
        Self {
            records,
            errors,
            integration_ok: true,
        }
    }
    fn failed(category: &'static str) -> Self {
        Self {
            records: vec![],
            errors: vec![Diagnostic {
                category,
                count: 1,
                verbose_path: None,
                verbose_chain: None,
            }],
            integration_ok: false,
        }
    }
}

fn discover_agent(agent: &str, scope: &Scope, cancel: &CancelToken) -> AgentDiscovery {
    if cancel.is_cancelled() {
        return AgentDiscovery::ok(vec![], vec![]);
    }
    match agent {
        "pi" => discover_pi(scope),
        "claude" => discover_claude(scope),
        "codex" => discover_codex(scope),
        "omp" => discover_omp(scope),
        _ => AgentDiscovery::failed("unknown_agent"),
    }
}

fn discover_pi(scope: &Scope) -> AgentDiscovery {
    let mut inputs = pi::ResolutionInputs::from_env();
    if let Some(root) = inputs.agent_dir_env.clone().or_else(|| {
        inputs
            .home
            .as_ref()
            .map(|h| h.join(pi::DEFAULT_AGENT_ROOT_RELATIVE))
    }) {
        inputs.settings = pi::read_settings(&root);
    }
    let Some(roots) = pi::resolve(&inputs) else {
        return AgentDiscovery::failed("pi_root_unavailable");
    };
    let config = pi::DiscoverConfig::new(roots.clone(), scope);
    match pi::discover(&config) {
        Ok(outcome) => {
            let records = outcome
                .parsed
                .into_iter()
                .map(|parsed| {
                    let spec = parsed.resume_spec(&roots);
                    let mut session = parsed.clone().into_session(
                        &roots,
                        pi::risk_status(&parsed, home().as_deref()),
                        pi::activity_status(&parsed, None),
                    );
                    normalize_availability(&mut session);
                    record(session, spec)
                })
                .collect();
            AgentDiscovery::ok(records, count_errors("pi_skipped", outcome.skipped_files))
        }
        Err(_) => AgentDiscovery::failed("pi_discovery_failed"),
    }
}

fn discover_claude(scope: &Scope) -> AgentDiscovery {
    let Some(root) = claude::resolve_root(
        std::env::var_os(claude::CONFIG_DIR_ENV).as_deref(),
        home().as_deref(),
    ) else {
        return AgentDiscovery::failed("claude_root_unavailable");
    };
    match claude::discover(&root) {
        Ok(discovery) => {
            let records = discovery
                .sessions
                .into_iter()
                .filter(|session| in_scope(scope, session))
                .map(|mut session| {
                    session.risk =
                        crate::scope::broad_workspace_risk(&session.workspace, home().as_deref());
                    normalize_availability(&mut session);
                    let spec = claude::resume_spec(&session, &root).ok();
                    record_optional(session, spec)
                })
                .collect();
            AgentDiscovery::ok(records, discovery.diagnostics)
        }
        Err(_) => AgentDiscovery::failed("claude_discovery_failed"),
    }
}

fn discover_codex(scope: &Scope) -> AgentDiscovery {
    let Some(root) = codex::effective_root() else {
        return AgentDiscovery::failed("codex_root_unavailable");
    };
    let outcomes = codex::discover_with_filter_enriched(&root, &Bounds::default(), |parsed| {
        parsed.cwd.as_ref().is_none_or(|cwd| {
            scope.contains(WorkspaceCandidate {
                real_path: cwd,
                git_common_dir: None,
                exists: cwd.exists(),
            })
        })
    });
    let mut errors = Vec::new();
    let default_root = home().unwrap_or_default().join(".codex");
    let records = outcomes
        .0
        .into_iter()
        .filter_map(|outcome| match outcome {
            codex::DiscoveredSession::Session(mut session) => {
                session.risk =
                    crate::scope::broad_workspace_risk(&session.workspace, home().as_deref());
                normalize_availability(&mut session);
                let spec = codex::resume_spec(&session, &default_root);
                let evidence = codex_transcript_path(&session)
                    .and_then(|path| LaunchEvidence::capture_with_transcript(&session, path).ok());
                Some(CandidateRecord {
                    session,
                    spec: Some(spec),
                    evidence,
                })
            }
            codex::DiscoveredSession::Error { error, .. } => {
                if let crate::session::IntegrationError::InvalidSession { diagnostic }
                | crate::session::IntegrationError::Io { diagnostic, .. } = error
                {
                    errors.push(diagnostic);
                }
                None
            }
        })
        .collect();
    AgentDiscovery::ok(records, errors)
}

fn discover_omp(scope: &Scope) -> AgentDiscovery {
    let base_inputs = omp::ResolutionInputs::from_env();
    let Some(base_roots) = omp::resolve(&base_inputs) else {
        return AgentDiscovery::failed("omp_root_unavailable");
    };
    let mut roots = vec![base_roots.clone()];
    let profiles = base_roots.config_root.join(omp::PROFILES_DIR_NAME);
    if let Ok(entries) = std::fs::read_dir(profiles) {
        for entry in entries
            .flatten()
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        {
            let mut inputs = base_inputs.clone();
            inputs.profile_flag = Some(entry.file_name());
            inputs.omp_profile_env = None;
            inputs.pi_profile_env = None;
            if let Some(profile_roots) = omp::resolve(&inputs)
                && !roots.iter().any(|r| r.profile == profile_roots.profile)
            {
                roots.push(profile_roots);
            }
        }
    }
    let mut records = Vec::new();
    let mut errors = Vec::new();
    for root in roots {
        match omp::discover(&omp::DiscoverConfig::new(root.clone(), scope)) {
            Ok(outcome) => {
                errors.extend(count_errors("omp_skipped", outcome.skipped_files));
                records.extend(outcome.parsed.into_iter().map(|parsed| {
                    let spec = parsed.resume_spec(&root);
                    let mut session = parsed.clone().into_session(
                        &root,
                        omp::risk_status(&parsed, home().as_deref()),
                        omp::activity_status(&parsed, None),
                    );
                    normalize_availability(&mut session);
                    record(session, spec)
                }));
            }
            Err(_) => errors.push(Diagnostic {
                category: "omp_discovery_failed",
                count: 1,
                verbose_path: None,
                verbose_chain: None,
            }),
        }
    }
    AgentDiscovery::ok(records, errors)
}

fn codex_transcript_path(session: &Session) -> Option<PathBuf> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let bytes = session.key.native_locator.as_os_str().as_bytes();
        let separator = bytes.windows(2).position(|window| window == b"::")?;
        Some(PathBuf::from(std::ffi::OsStr::from_bytes(
            &bytes[separator + 2..],
        )))
    }
    #[cfg(not(unix))]
    {
        session
            .key
            .native_locator
            .to_string_lossy()
            .split_once("::")
            .map(|(_, path)| PathBuf::from(path))
    }
}

fn record(session: Session, spec: ResumeSpec) -> CandidateRecord {
    record_optional(session, Some(spec))
}
fn record_optional(session: Session, spec: Option<ResumeSpec>) -> CandidateRecord {
    let evidence = if session.support == SupportStatus::Supported {
        LaunchEvidence::capture(&session).ok()
    } else {
        None
    };
    CandidateRecord {
        session,
        spec,
        evidence,
    }
}
fn normalize_availability(session: &mut Session) {
    if session.workspace.workspace().is_none_or(|p| !p.is_dir()) {
        session.support = SupportStatus::Unavailable;
    }
}
fn in_scope(scope: &Scope, session: &Session) -> bool {
    session.workspace.workspace().is_none_or(|workspace| {
        let canonical = workspace.canonicalize().ok();
        scope.contains(WorkspaceCandidate {
            real_path: canonical.as_deref().unwrap_or(workspace),
            git_common_dir: None,
            exists: canonical.is_some(),
        })
    })
}
fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}
fn count_errors(category: &'static str, count: usize) -> Vec<Diagnostic> {
    if count == 0 {
        vec![]
    } else {
        vec![Diagnostic {
            category,
            count,
            verbose_path: None,
            verbose_chain: None,
        }]
    }
}

fn picker_candidate(key: CandidateKey, session: &Session) -> PickerCandidate {
    let agent = agent_label(session);
    let status = status_label(session);
    let updated = activity_label(&session.activity);
    let title = session.title.as_deref().unwrap_or("<untitled>");
    let workspace = session
        .workspace
        .workspace()
        .map_or_else(|| "<missing>".into(), |p| p.display().to_string());
    let branch = "-";
    let display = format!(
        "{:<9} {:<18} {:<10} {} {} {}",
        status,
        agent,
        updated,
        text::truncate_to_width(title, 48),
        branch,
        text::truncate_to_width(&workspace, 48)
    );
    let search_text = text::normalize(
        &format!(
            "{status} {agent} {updated} {title} {branch} {workspace} {:?}",
            session.resumable_id
        ),
        text::Mode::Normalized,
    );
    let preview = text::normalize(
        &format!(
            "STATUS {status}\nAGENT {agent}\nUPDATED {updated}\nTITLE {title}\nBRANCH {branch}\nWORKSPACE {workspace}\n\n# normalized\n{title}\n\n# raw (still terminal-safe)\n{title}"
        ),
        text::Mode::Raw,
    );
    PickerCandidate {
        key,
        display,
        search_text,
        preview,
    }
}
fn agent_label(session: &Session) -> String {
    match &session.key.profile {
        Some(profile) => format!(
            "{}[{}]",
            session.key.agent.to_string_lossy(),
            profile.to_string_lossy()
        ),
        None => session.key.agent.to_string_lossy().into_owned(),
    }
}
fn status_label(session: &Session) -> &'static str {
    match session.support {
        SupportStatus::Supported => {
            if matches!(session.activity, ActivityStatus::Active { .. }) {
                "ACTIVE"
            } else {
                "READY"
            }
        }
        SupportStatus::DiscoverOnly => "DISCOVER",
        SupportStatus::Unsupported => "UNSUPPORTED",
        SupportStatus::Unavailable => "UNAVAILABLE",
    }
}
fn activity_label(activity: &ActivityStatus) -> String {
    match activity {
        ActivityStatus::Active { observed_at } | ActivityStatus::Inactive { observed_at } => {
            observed_at
                .duration_since(std::time::UNIX_EPOCH)
                .map_or_else(|_| "?".into(), |d| d.as_secs().to_string())
        }
        ActivityStatus::Unknown => "unknown".into(),
    }
}

#[derive(Serialize)]
struct JsonOutput<'a> {
    #[serde(rename = "schemaVersion")]
    schema_version: u8,
    sessions: Vec<JsonSession>,
    errors: Vec<JsonError<'a>>,
}
#[derive(Serialize)]
struct JsonSession {
    agent: String,
    profile: Option<String>,
    id: String,
    title: Option<String>,
    workspace: Option<String>,
    support: String,
    activity: String,
    risk: String,
}
#[derive(Serialize)]
struct JsonError<'a> {
    category: &'a str,
    count: usize,
}
fn print_json(records: &[CandidateRecord], state: &DiscoveryState) {
    let sessions = records
        .iter()
        .map(|r| JsonSession {
            agent: r.session.key.agent.to_string_lossy().into_owned(),
            profile: r
                .session
                .key
                .profile
                .as_ref()
                .map(|p| p.to_string_lossy().into_owned()),
            id: r.session.resumable_id.to_string_lossy().into_owned(),
            title: r.session.title.clone(),
            workspace: r
                .session
                .workspace
                .workspace()
                .map(|p| p.display().to_string()),
            support: format!("{:?}", r.session.support),
            activity: format!("{:?}", r.session.activity),
            risk: format!("{:?}", r.session.risk),
        })
        .collect();
    let errors_guard = state.errors.lock().unwrap();
    let errors = errors_guard
        .iter()
        .map(|e| JsonError {
            category: e.category,
            count: e.count,
        })
        .collect();
    println!(
        "{}",
        serde_json::to_string(&JsonOutput {
            schema_version: 1,
            sessions,
            errors
        })
        .expect("JSON serialization")
    );
}
fn print_list(records: &[CandidateRecord]) {
    for record in records {
        println!(
            "{}",
            picker_candidate(CandidateKey(0), &record.session).display
        );
    }
}
fn print_diagnostics(state: &DiscoveryState, verbose: bool) {
    for error in aggregate_diagnostics(&state.errors.lock().unwrap()) {
        eprintln!("{}", render_diagnostic(&error, verbose));
    }
}

/// Collapse diagnostics into one entry per distinct `(category, verbose_path,
/// verbose_chain)`, summing counts. A `Diagnostic` is defined as "redacted
/// category, count, optional verbose path/error chain" (see
/// `plans/v0.1.0-implementation.md`), so N occurrences of the same shape must
/// render as one line with `count: N`, never N duplicate lines.
fn aggregate_diagnostics(errors: &[Diagnostic]) -> Vec<Diagnostic> {
    let mut order: Vec<(&'static str, Option<PathBuf>, Option<String>)> = Vec::new();
    let mut totals: HashMap<(&'static str, Option<PathBuf>, Option<String>), usize> =
        HashMap::new();
    for error in errors {
        let key = (
            error.category,
            error.verbose_path.clone(),
            error.verbose_chain.clone(),
        );
        if !totals.contains_key(&key) {
            order.push(key.clone());
        }
        *totals.entry(key).or_insert(0) += error.count;
    }
    order
        .into_iter()
        .map(|key| {
            let count = totals[&key];
            Diagnostic {
                category: key.0,
                count,
                verbose_path: key.1,
                verbose_chain: key.2,
            }
        })
        .collect()
}

fn render_diagnostic(error: &Diagnostic, verbose: bool) -> String {
    if !verbose {
        return format!("resume: {}: {}", error.category, error.count);
    }

    let path = error
        .verbose_path
        .as_deref()
        .map(redact_path)
        .unwrap_or_default();
    let chain = error
        .verbose_chain
        .as_deref()
        .map(redact_text)
        .unwrap_or_default();
    format!(
        "resume: {}: {} {} {}",
        error.category, error.count, path, chain
    )
}
fn discovery_exit(records: &[CandidateRecord], state: &DiscoveryState) -> i32 {
    if records.is_empty() && state.successful_integrations.load(Ordering::SeqCst) == 0 {
        EXIT_ERROR
    } else {
        EXIT_OK
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{RiskStatus, WorkspaceEvidence};
    use std::ffi::OsString;
    #[test]
    fn row_priority_and_search_keep_full_fields() {
        let session = Session {
            key: crate::session::SessionKey {
                agent: "omp".into(),
                effective_root: "/r".into(),
                profile: Some("work".into()),
                native_locator: "/t".into(),
            },
            resumable_id: "id".into(),
            title: Some("title".into()),
            workspace: WorkspaceEvidence::Recorded {
                workspace: "/workspace".into(),
                historical_git_identity: None,
            },
            support: SupportStatus::Supported,
            activity: ActivityStatus::Unknown,
            risk: RiskStatus::Normal,
        };
        let item = picker_candidate(CandidateKey(1), &session);
        assert!(item.display.starts_with("READY     omp[work]"));
        assert!(item.search_text.contains("/workspace"));
    }
    #[test]
    fn verbose_diagnostic_output_is_redacted() {
        let diagnostic = Diagnostic {
            category: "io_error",
            count: 1,
            verbose_path: Some(PathBuf::from(
                "/sessions/https://secret.example/transcript.jsonl",
            )),
            verbose_chain: Some(
                "failed fetching git@github.com:private/repo.git https://secret.example/api".into(),
            ),
        };

        let rendered = render_diagnostic(&diagnostic, true);
        assert!(!rendered.contains("secret.example"));
        assert!(!rendered.contains("github.com"));
        assert!(rendered.contains("[redacted-url]"));
        assert!(rendered.contains("[redacted-remote]"));
    }

    #[test]
    fn unknown_agent_is_usage_error() {
        let cli = Cli {
            directory: None,
            up: None,
            down: None,
            agent: vec![OsString::from("bad")],
            list: false,
            json: false,
            verbose: false,
            config: None,
            confirm_always: false,
            no_confirm: false,
            command: None,
        };
        assert!(effective_options(&cli, Config::default()).is_err());
    }
}
