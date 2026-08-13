//! Top-level discovery, picker/list output, revalidation, confirmation, and exec orchestration.

use std::{
    collections::HashMap,
    io,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    thread,
    time::Duration,
};

use serde::Serialize;

use crate::{
    cli::{Cli, SUPPORTED_AGENTS},
    config::{Config, PreviewMode, PreviewPosition},
    diagnostics::{redact_path, redact_text},
    integration::{claude, codex, omp, opencode, pi},
    launch::{self, LaunchEvidence},
    picker::{CandidateKey, PickerCandidate, PickerOutcome},
    preview::{jsonl::Bounds, text},
    runtime::CancelToken,
    scope::{DefaultScope, Direction, Scope},
    session::{Diagnostic, ResumeSpec, Session, SupportStatus},
    settings,
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
    since_cutoff: Option<std::time::SystemTime>,
    confirm_always: bool,
    no_confirm: bool,
    preview: PreviewMode,
    preview_position: PreviewPosition,
    verbose: bool,
}

/// Read-only process evidence shared by all discovery workers.
struct DiscoveryContext {
    procs: crate::proc::ProcessTable,
    codex_activity: codex::activity::ActivitySnapshot,
    diagnostics: Vec<Diagnostic>,
}

impl DiscoveryContext {
    /// Probes read-only process/TTY state for OMP/Pi and Codex's rollout-fd
    /// activity evidence in one pass, each gated per agent so a run that
    /// excludes an agent never pays its probe cost. The context owns every
    /// probe diagnostic so evidence and diagnostics cannot desynchronize.
    fn probe(options: &EffectiveOptions) -> Self {
        let needed = options
            .agents
            .iter()
            .any(|agent| agent == "omp" || agent == "pi");
        let (procs, mut diagnostics) = if needed {
            crate::proc::snapshot()
        } else {
            (crate::proc::ProcessTable::empty(), Vec::new())
        };
        let (codex_activity, codex_diagnostics) =
            if options.agents.iter().any(|agent| agent == codex::AGENT) {
                codex::activity::probe()
            } else {
                (codex::activity::ActivitySnapshot::empty(), Vec::new())
            };
        diagnostics.extend(codex_diagnostics);
        Self {
            procs,
            codex_activity,
            diagnostics,
        }
    }
}

pub fn run(cli: Cli) -> i32 {
    let (config, _) = match crate::config::load(cli.config.clone()) {
        Ok(value) => value,
        Err(error) => {
            return crate::errors::E1004.report_with(error).emit();
        }
    };
    let (settings, new_agents) = if cli.agent.is_empty() && config.agents.is_none() {
        match settings::load_or_setup() {
            Ok(value) => value,
            Err(error) => {
                eprintln!("resume: {error}");
                return EXIT_USAGE;
            }
        }
    } else {
        // An explicit CLI or TOML selection has precedence over settings and
        // must remain usable even when persisted settings were written by an
        // invalid or incompatible version.
        (settings::Settings::default(), Vec::new())
    };
    if !new_agents.is_empty() {
        eprintln!(
            "resume: new supported agent{} available: {}; run `resume setup` to enable {}",
            if new_agents.len() == 1 { "" } else { "s" },
            new_agents.join(", "),
            if new_agents.len() == 1 { "it" } else { "them" },
        );
    }
    let options = match effective_options(&cli, config, settings) {
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
    let discovery_ctx = Arc::new(DiscoveryContext::probe(&options));

    if cli.list || cli.json {
        let (records, state) = discover_all(&options, scope, discovery_ctx);
        if cli.json {
            print_diagnostics(&state, options.verbose);
            print_json(&records, &state);
        } else {
            print_list(&records);
            print_diagnostics(&state, options.verbose);
        }
        return discovery_exit(&records, &state, options.agents.is_empty());
    }

    run_interactive(&options, scope, discovery_ctx)
}

fn effective_options(
    cli: &Cli,
    config: Config,
    settings: settings::Settings,
) -> Result<EffectiveOptions, String> {
    let agents: Vec<String> = if !cli.agent.is_empty() {
        cli.agent
            .iter()
            .map(|a| a.to_string_lossy().to_ascii_lowercase())
            .collect()
    } else if let Some(agents) = config.agents {
        agents
    } else {
        settings.agents().to_vec()
    };
    for agent in &agents {
        if !SUPPORTED_AGENTS.contains(&agent.as_str()) {
            return Err(format!("unknown agent {agent:?}"));
        }
    }
    // CLI `--since` replaces a configured `since`, matching the `-a/--agent`
    // precedence pattern above. Absent both, there is no cutoff (equivalent
    // to `all`).
    let since_cutoff = cli
        .since
        .clone()
        .or_else(|| config.since.clone())
        .and_then(|since| since.cutoff(std::time::SystemTime::now()));
    Ok(EffectiveOptions {
        agents,
        since_cutoff,
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
        match crate::scope::discover_git_scope(&base, cli.all_worktrees) {
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
    ctx: Arc<DiscoveryContext>,
) -> (Vec<CandidateRecord>, Arc<DiscoveryState>) {
    let state = Arc::new(DiscoveryState::default());
    if let Some(diagnostic) = scope_warning_diagnostic(scope.git_warning().map(str::to_owned)) {
        state.errors.lock().unwrap().push(diagnostic);
    }
    state.errors.lock().unwrap().extend(ctx.diagnostics.clone());
    let records = Arc::new(Mutex::new(Vec::new()));
    let cancel = CancelToken::new();
    let mut handles = Vec::new();
    for agent in &options.agents {
        let agent = agent.clone();
        let scope = scope.clone();
        let ctx = ctx.clone();
        let state = state.clone();
        let records = records.clone();
        let cancel = cancel.clone();
        let since_cutoff = options.since_cutoff;
        handles.push(thread::spawn(move || {
            let result = discover_agent(&agent, &scope, &ctx, since_cutoff, &cancel);
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

fn run_interactive(
    options: &EffectiveOptions,
    scope: Arc<Scope>,
    ctx: Arc<DiscoveryContext>,
) -> i32 {
    let state = Arc::new(DiscoveryState::default());
    if let Some(diagnostic) = scope_warning_diagnostic(scope.git_warning().map(str::to_owned)) {
        state.errors.lock().unwrap().push(diagnostic);
    }
    state.errors.lock().unwrap().extend(ctx.diagnostics.clone());
    let cancel = CancelToken::new();

    let next_key = Arc::new(AtomicU64::new(1));
    let map: Arc<Mutex<HashMap<CandidateKey, CandidateRecord>>> =
        Arc::new(Mutex::new(HashMap::new()));
    let candidates: Arc<Mutex<Vec<PickerCandidate>>> = Arc::new(Mutex::new(Vec::new()));

    // Codex's discovery cost is dominated by per-file JSONL parsing and, on
    // a large real corpus, is not bounded the way the directory-pruned
    // pi/omp/claude scans are (observed: sub-second for those three,
    // single-digit to tens of seconds for Codex). When at least one other
    // agent is configured, Codex discovers in the background instead of
    // holding the picker closed: the picker opens on the other agents'
    // results, and Codex's Sessions merge in on the next tab switch or
    // page turn once its scan finishes (`picker::run_tabbed_picker`
    // re-reads the shared candidate list on every navigation). When Codex
    // is the *only* configured agent there is nothing else to show while
    // waiting, so it stays synchronous like every other agent.
    let codex_async = options.agents.iter().any(|a| a == codex::AGENT) && options.agents.len() > 1;
    let sync_agents: Vec<&String> = options
        .agents
        .iter()
        .filter(|a| !codex_async || a.as_str() != codex::AGENT)
        .collect();

    let (progress_tx, progress_rx) = std::sync::mpsc::channel::<(String, Duration)>();
    let mut handles = Vec::new();
    for agent in sync_agents {
        let agent = agent.clone();
        let scope = scope.clone();
        let ctx = ctx.clone();
        let state = state.clone();
        let cancel = cancel.clone();
        let since_cutoff = options.since_cutoff;
        let progress_tx = progress_tx.clone();
        let next_key = next_key.clone();
        let map = map.clone();
        let candidates = candidates.clone();
        handles.push(thread::spawn(move || {
            let start = std::time::Instant::now();
            let result = discover_agent(&agent, &scope, &ctx, since_cutoff, &cancel);
            if result.integration_ok {
                state.successful_integrations.fetch_add(1, Ordering::SeqCst);
            }
            state
                .sessions
                .fetch_add(result.records.len(), Ordering::SeqCst);
            state.errors.lock().unwrap().extend(result.errors);
            merge_records(result.records, &next_key, &map, &candidates);
            let _ = progress_tx.send((agent, start.elapsed()));
        }));
    }
    drop(progress_tx);
    // Print progress in real completion order (not spawn order): each
    // thread sends its one message right after its own work finishes, so
    // draining the channel to closure reports agents as they actually
    // settle, before we ever open the picker.
    for (agent, elapsed) in progress_rx {
        eprintln!("resume: {agent} scanned ({elapsed:.2?})");
    }
    for handle in handles {
        let _ = handle.join();
    }

    let codex_pending = Arc::new(AtomicBool::new(codex_async));
    let (codex_progress_tx, codex_progress_rx) = std::sync::mpsc::channel::<Duration>();
    if codex_async {
        let scope = scope.clone();
        let ctx = ctx.clone();
        let state = state.clone();
        let cancel = cancel.clone();
        let since_cutoff = options.since_cutoff;
        let next_key = next_key.clone();
        let map = map.clone();
        let candidates = candidates.clone();
        let codex_pending = codex_pending.clone();
        // Detached on purpose: never joined before the picker opens (that
        // would reintroduce the exact block this split avoids), and never
        // joined after either -- once the user has an outcome the process
        // exits shortly after, which reaps this thread regardless.
        thread::spawn(move || {
            let start = std::time::Instant::now();
            let result = discover_agent(codex::AGENT, &scope, &ctx, since_cutoff, &cancel);
            if result.integration_ok {
                state.successful_integrations.fetch_add(1, Ordering::SeqCst);
            }
            state
                .sessions
                .fetch_add(result.records.len(), Ordering::SeqCst);
            state.errors.lock().unwrap().extend(result.errors);
            merge_records(result.records, &next_key, &map, &candidates);
            codex_pending.store(false, Ordering::SeqCst);
            let _ = codex_progress_tx.send(start.elapsed());
        });
    }

    let background = codex_async.then(|| crate::picker::BackgroundAgent {
        label: codex::AGENT.to_string(),
        pending: codex_pending,
    });
    let outcome = crate::picker::run_tabbed_picker(
        candidates,
        options.preview,
        options.preview_position,
        background,
    );
    // Codex's own progress line cannot be printed while it might race
    // Skim's raw-mode rendering, so it is buffered and only flushed once
    // the picker has released the terminal. A still-running background
    // scan (the user acted before Codex finished) prints nothing here --
    // there is no result to time yet, and waiting for one would
    // reintroduce exactly the blocking this design avoids.
    if let Ok(elapsed) = codex_progress_rx.try_recv() {
        eprintln!("resume: codex scanned ({elapsed:.2?})");
    }
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
        PickerOutcome::PreflightFailed(reason) => {
            eprintln!("resume: {reason}");
            if reason.contains("no controlling terminal") {
                eprintln!("resume: use --list or --json in this environment");
            }
            EXIT_USAGE
        }
        PickerOutcome::InternalError(reason) => {
            eprintln!("resume: {reason}");
            EXIT_ERROR
        }
        PickerOutcome::Selected(key) => {
            let Some(record) = map.lock().unwrap().remove(&key) else {
                eprintln!("resume: selected Session disappeared");
                return EXIT_ERROR;
            };
            resume_selected(record, options)
        }
    }
}

/// Fold freshly discovered records into the shared, live candidate list:
/// assign each an opaque key, insert it into `map` (resolvable by
/// `PickerOutcome::Selected`) before it is ever visible in `candidates`
/// (picked up by `picker::run_tabbed_picker` on its next navigation), then
/// re-sort `candidates` once for the whole batch. Called once per sync
/// agent batch and once when the background Codex scan finishes.
fn merge_records(
    records: Vec<CandidateRecord>,
    next_key: &AtomicU64,
    map: &Mutex<HashMap<CandidateKey, CandidateRecord>>,
    candidates: &Mutex<Vec<PickerCandidate>>,
) {
    if records.is_empty() {
        return;
    }
    let mut new_candidates = Vec::with_capacity(records.len());
    {
        let mut map = map.lock().unwrap();
        for record in records {
            let key = CandidateKey(next_key.fetch_add(1, Ordering::SeqCst));
            new_candidates.push(picker_candidate(key.clone(), &record.session));
            map.insert(key, record);
        }
    }
    let mut candidates = candidates.lock().unwrap();
    candidates.extend(new_candidates);
    candidates.sort_by(|a, b| a.rank.cmp(&b.rank).then_with(|| a.key.0.cmp(&b.key.0)));
}

fn resume_selected(record: CandidateRecord, options: &EffectiveOptions) -> i32 {
    if record.session.support != SupportStatus::Supported {
        return crate::errors::E3003
            .report(format!(
                "selected Session is unavailable: {:?}",
                record.session.support
            ))
            .emit();
    }
    let (Some(spec), Some(evidence)) = (record.spec.as_ref(), record.evidence.as_ref()) else {
        return crate::errors::E3003
            .report("selected Session cannot be resumed")
            .emit();
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
        Self::failed_with_errors(vec![Diagnostic {
            category,
            count: 1,
            verbose_path: None,
            verbose_chain: None,
        }])
    }

    fn failed_with_errors(errors: Vec<Diagnostic>) -> Self {
        Self {
            records: vec![],
            errors,
            integration_ok: false,
        }
    }
}

fn discover_agent(
    agent: &str,
    scope: &Scope,
    ctx: &DiscoveryContext,
    since_cutoff: Option<std::time::SystemTime>,
    cancel: &CancelToken,
) -> AgentDiscovery {
    if cancel.is_cancelled() {
        return AgentDiscovery::ok(vec![], vec![]);
    }
    let mut discovery = match agent {
        "pi" => discover_pi(scope, ctx),
        "claude" => discover_claude(scope),
        "codex" => discover_codex(scope, &ctx.codex_activity),
        "omp" => discover_omp(scope, ctx),
        "opencode" => discover_opencode(scope),
        _ => AgentDiscovery::failed("unknown_agent"),
    };
    if let Some(cutoff) = since_cutoff {
        discovery
            .records
            .retain(|record| session_at_or_after(record, cutoff));
    }
    discovery
}

/// `--since` filter: keep a Session only when its `updated_at` (native last
/// activity time, falling back to the transcript file's own mtime only when
/// no native timestamp is available — the same signal each integration
/// already computes for the `UPDATED` display column) is at or after
/// `cutoff`. Per docs/product-design.md §7 ("Use native last activity time,
/// then documented fallback. When `--since` is active, exclude unknown-time
/// Sessions."), a Session with no resolvable activity time is excluded, not
/// conservatively kept.
fn session_at_or_after(record: &CandidateRecord, cutoff: std::time::SystemTime) -> bool {
    record
        .session
        .updated_at
        .is_some_and(|update| update.at >= cutoff)
}

fn discover_pi(scope: &Scope, ctx: &DiscoveryContext) -> AgentDiscovery {
    let _ = ctx;
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
    let home_dir = home();
    match claude::discover_with_dir_filter(&root, |name| {
        scope.may_contain_session_dir(name, home_dir.as_deref())
    }) {
        Ok(discovery) => {
            let mut diagnostics = discovery.diagnostics;
            let records = discovery
                .sessions
                .into_iter()
                .filter(|session| in_scope(scope, session))
                .map(|mut session| {
                    session.risk =
                        crate::scope::broad_workspace_risk(&session.workspace, home().as_deref());
                    normalize_availability(&mut session);
                    let spec = match claude::resume_spec(&session, &root) {
                        Ok(spec) => Some(spec),
                        Err(crate::session::IntegrationError::InvalidSession { diagnostic })
                        | Err(crate::session::IntegrationError::Io { diagnostic, .. }) => {
                            diagnostics.push(diagnostic);
                            None
                        }
                        Err(crate::session::IntegrationError::Unavailable) => None,
                    };
                    record_optional(session, spec)
                })
                .collect();
            if diagnostics
                .iter()
                .any(|diagnostic| diagnostic.category == "claude_root_unavailable")
            {
                AgentDiscovery::failed_with_errors(diagnostics)
            } else {
                AgentDiscovery::ok(records, diagnostics)
            }
        }
        Err(_) => AgentDiscovery::failed("claude_discovery_failed"),
    }
}

fn discover_codex(scope: &Scope, activity: &codex::activity::ActivitySnapshot) -> AgentDiscovery {
    let Some(root) = codex::effective_root() else {
        return AgentDiscovery::failed("codex_root_unavailable");
    };
    let cache_path = codex::cache::cache_path(
        std::env::var_os("XDG_CACHE_HOME").map(PathBuf::from),
        home(),
    );
    let cache = codex::cache::DiscoveryCache::load(cache_path);
    let workspace_gate = |cwd: &Path| scope.contains_workspace(cwd);
    let (outcomes, sqlite_outcome) = codex::discover_with_filter_enriched(
        &root,
        &Bounds::default(),
        Some(&workspace_gate),
        |parsed| {
            parsed
                .cwd
                .as_ref()
                .is_none_or(|cwd| scope.contains_workspace(cwd))
        },
        Some(&cache),
    );
    let mut errors = match sqlite_outcome {
        codex::sqlite::SqliteOutcome::Used { diagnostics, .. } => diagnostics,
        codex::sqlite::SqliteOutcome::Degraded { category } => vec![Diagnostic {
            category,
            count: 1,
            verbose_path: Some(codex::sqlite::state_db_path(&root)),
            verbose_chain: Some("SQLite enrichment degraded; using authoritative JSONL".into()),
        }],
        codex::sqlite::SqliteOutcome::Absent => Vec::new(),
    };
    let default_root = home().unwrap_or_default().join(".codex");
    let records = outcomes
        .into_iter()
        .filter_map(|outcome| match outcome {
            codex::DiscoveredSession::Session(mut session) => {
                session.risk =
                    crate::scope::broad_workspace_risk(&session.workspace, home().as_deref());
                normalize_availability(&mut session);
                if let Some(rollout) = codex_transcript_path(&session) {
                    session.activity = codex::activity::activity_status(&rollout, Some(activity));
                }
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
    if errors
        .iter()
        .any(|diagnostic| diagnostic.category == "codex_root_unavailable")
    {
        AgentDiscovery::failed_with_errors(errors)
    } else {
        AgentDiscovery::ok(records, errors)
    }
}

fn discover_omp(scope: &Scope, ctx: &DiscoveryContext) -> AgentDiscovery {
    let base_inputs = omp::ResolutionInputs::from_env();
    let Some(base_roots) = omp::resolve(&base_inputs) else {
        return AgentDiscovery::failed("omp_root_unavailable");
    };
    let mut roots = vec![base_roots.clone()];
    // `base_roots` already reflects OMP_PROFILE/PI_PROFILE env selection, so
    // when one of them points at a named profile, the true unprofiled
    // default is never otherwise resolved — silently dropping it from "all
    // profiles" discovery. Force-resolve Default separately and always
    // include it, independent of which profile the env vars selected.
    let mut default_inputs = base_inputs.clone();
    default_inputs.profile_flag = None;
    default_inputs.omp_profile_env = None;
    default_inputs.pi_profile_env = None;
    if let Some(default_roots) = omp::resolve(&default_inputs)
        && !roots.iter().any(|r| r.profile == default_roots.profile)
    {
        roots.push(default_roots);
    }
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
    let (live, mut errors) = omp::correlate_live(&ctx.procs, &roots);
    let mut records = Vec::new();
    for root in roots {
        match omp::discover(&omp::DiscoverConfig::new(root.clone(), scope)) {
            Ok(outcome) => {
                errors.extend(count_errors("omp_skipped", outcome.skipped_files));
                records.extend(outcome.parsed.into_iter().map(|parsed| {
                    let spec = parsed.resume_spec(&root);
                    let mut session = parsed.clone().into_session(
                        &root,
                        omp::risk_status(&parsed, home().as_deref()),
                        omp::activity_status(&parsed, live.for_transcript(&parsed.transcript_path)),
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

fn discover_opencode(scope: &Scope) -> AgentDiscovery {
    let Some(root) = opencode::roots::effective_root() else {
        return AgentDiscovery::failed("opencode_root_unavailable");
    };
    match opencode::discover(&root) {
        Ok(Some(outcome)) => {
            let errors = count_errors("opencode_skipped", outcome.skipped_rows);
            let home_dir = home();
            let records = outcome
                .parsed
                .into_iter()
                .filter(|parsed| scope.contains_workspace(&parsed.directory))
                .filter_map(|parsed| {
                    let mut session = parsed.clone().into_session(&root, home_dir.as_deref());
                    normalize_availability(&mut session);
                    let spec = opencode::resume_spec(&parsed).ok()?;
                    let transcript = opencode::transcript_path(&root);
                    let evidence =
                        LaunchEvidence::capture_with_transcript(&session, transcript).ok();
                    Some(CandidateRecord {
                        session,
                        spec: Some(spec),
                        evidence,
                    })
                })
                .collect();
            AgentDiscovery::ok(records, errors)
        }
        Ok(None) => AgentDiscovery::failed("opencode_root_unavailable"),
        Err(_) => AgentDiscovery::failed("opencode_discovery_failed"),
    }
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
    session
        .workspace
        .workspace()
        .is_none_or(|workspace| scope.contains_workspace(workspace))
}
fn scope_warning_diagnostic(warning: Option<String>) -> Option<Diagnostic> {
    warning.map(|warning| Diagnostic {
        category: "git_scope_discovery_failed",
        count: 1,
        verbose_path: None,
        verbose_chain: Some(warning),
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

/// Title column budget for the compact human list.
const TITLE_WIDTH_MIN: usize = 16;
const TITLE_WIDTH_MAX: usize = 60;
const TITLE_WIDTH_DEFAULT: usize = 48;

fn title_column_width() -> usize {
    const LEADING_COLUMNS: usize = 10 + 1 + 18 + 1;
    match crate::picker::tty_size() {
        Some((width, _)) if width > LEADING_COLUMNS => {
            (width - LEADING_COLUMNS).clamp(TITLE_WIDTH_MIN, TITLE_WIDTH_MAX)
        }
        _ => TITLE_WIDTH_DEFAULT,
    }
}

fn picker_candidate(key: CandidateKey, session: &Session) -> PickerCandidate {
    let agent = agent_label(session);
    let updated = updated_label(session.updated_at);
    let updated_detail = updated_detail(session.updated_at);
    // Native titles (e.g. Pi's session_info.name) are untrusted transcript
    // data and reach --list/JSON directly (unlike search_text/preview below,
    // which already normalize their own format! output) — sanitize before
    // any other use so no escape sequence can reach a terminal via `display`.
    let title = text::normalize(
        session.title.as_deref().unwrap_or("<untitled>"),
        text::Mode::Normalized,
    );
    let title = title.as_str();
    let branch = branch_label(session.workspace.workspace());
    let column_width = title_column_width();
    let display = format!(
        "{:<10} {:<18} {} {}",
        updated,
        agent,
        text::pad_to_width(&text::truncate_to_width(title, column_width), column_width),
        branch,
    );
    let search_text = text::normalize(
        &format!(
            "{updated} {agent} {title} {branch} {:?}",
            session.resumable_id
        ),
        text::Mode::Normalized,
    );
    let preview = text::normalize(
        &format!(
            "UPDATED {updated_detail}\nAGENT {agent}\nTITLE {title}\nWORKTREE {branch}\n\n# normalized\n{title}\n\n# raw (still terminal-safe)\n{title}"
        ),
        text::Mode::Raw,
    );
    PickerCandidate {
        key,
        display,
        search_text,
        preview,
        rank: crate::session::sort_rank(session.updated_at),
        agent: session.key.agent.to_string_lossy().into_owned(),
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

fn updated_label(updated_at: Option<crate::session::UpdateTime>) -> String {
    let Some(updated_at) = updated_at else {
        return "unknown".into();
    };
    let age = std::time::SystemTime::now()
        .duration_since(updated_at.at)
        .unwrap_or_default();
    if age < std::time::Duration::from_secs(60 * 60) {
        return format!("{}m", age.as_secs() / 60);
    }
    if age < std::time::Duration::from_secs(24 * 60 * 60) {
        return format!("{}h", age.as_secs() / (60 * 60));
    }
    if age < std::time::Duration::from_secs(7 * 24 * 60 * 60) {
        return format!("{}d", age.as_secs() / (24 * 60 * 60));
    }
    local_date_label(updated_at.at)
}

fn updated_detail(updated_at: Option<crate::session::UpdateTime>) -> String {
    let Some(updated_at) = updated_at else {
        return "unknown (unavailable)".into();
    };
    format!(
        "{} ({})",
        local_timestamp(updated_at.at),
        match updated_at.source {
            crate::session::UpdateTimeSource::Native => "native timestamp",
            crate::session::UpdateTimeSource::FileMtime => "file modification time",
        }
    )
}

fn local_date_label(at: std::time::SystemTime) -> String {
    let timestamp = local_timestamp(at);
    let current_timestamp = local_timestamp(std::time::SystemTime::now());
    let current_year = current_timestamp.get(..4).unwrap_or_default();
    if timestamp.starts_with(current_year) {
        timestamp.get(5..10).unwrap_or("unknown").into()
    } else {
        timestamp.get(..10).unwrap_or("unknown").into()
    }
}

fn local_timestamp(at: std::time::SystemTime) -> String {
    let seconds = at
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs());
    let output = if cfg!(target_os = "macos") {
        std::process::Command::new("date")
            .args(["-r", &seconds.to_string(), "+%Y-%m-%d %H:%M:%S %z"])
            .output()
    } else {
        std::process::Command::new("date")
            .args(["-d", &format!("@{seconds}"), "+%Y-%m-%d %H:%M:%S %z"])
            .output()
    };
    output
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .filter(|timestamp| !timestamp.is_empty())
        .unwrap_or_else(|| "unknown".into())
}

fn branch_label(workspace: Option<&std::path::Path>) -> String {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<HashMap<PathBuf, String>>> =
        std::sync::OnceLock::new();
    let Some(workspace) = workspace else {
        return "no-branch".into();
    };
    let cache = CACHE.get_or_init(|| std::sync::Mutex::new(HashMap::new()));
    if let Ok(cache) = cache.lock()
        && let Some(branch) = cache.get(workspace)
    {
        return branch.clone();
    }
    let branch = resolve_branch_label(workspace);
    if let Ok(mut cache) = cache.lock() {
        cache.insert(workspace.to_path_buf(), branch.clone());
    }
    branch
}

fn resolve_branch_label(workspace: &std::path::Path) -> String {
    let Ok(output) = std::process::Command::new("git")
        .args([
            std::ffi::OsStr::new("-C"),
            workspace.as_os_str(),
            std::ffi::OsStr::new("symbolic-ref"),
            std::ffi::OsStr::new("--quiet"),
            std::ffi::OsStr::new("--short"),
            std::ffi::OsStr::new("HEAD"),
        ])
        .output()
    else {
        return "no-branch".into();
    };
    if output.status.success() {
        let branch = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if !branch.is_empty() {
            return branch;
        }
    }
    if std::process::Command::new("git")
        .args([
            std::ffi::OsStr::new("-C"),
            workspace.as_os_str(),
            std::ffi::OsStr::new("rev-parse"),
            std::ffi::OsStr::new("--is-inside-work-tree"),
        ])
        .output()
        .is_ok_and(|output| output.status.success())
    {
        "detached".into()
    } else {
        "no-branch".into()
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
            title: r
                .session
                .title
                .as_deref()
                .map(|t| text::normalize(t, text::Mode::Normalized)),
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
    let aggregated = aggregate_diagnostics(&errors_guard, false);
    let errors = aggregated
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
    if let Some(message) = empty_list_message(records) {
        println!("{message}");
        return;
    }
    for record in records {
        println!(
            "{}",
            picker_candidate(CandidateKey(0), &record.session).display
        );
    }
}

fn empty_list_message(records: &[CandidateRecord]) -> Option<&'static str> {
    records.is_empty().then_some("No Sessions found in Scope.")
}
fn print_diagnostics(state: &DiscoveryState, verbose: bool) {
    for error in aggregate_diagnostics(&state.errors.lock().unwrap(), verbose) {
        eprintln!("{}", render_diagnostic(&error, verbose));
    }
}

/// Collapse diagnostics for stderr rendering. A `Diagnostic` is defined as
/// "redacted category, count, optional verbose path/error chain" (see
/// `plans/v0.1.0-implementation.md`): repeated occurrences of the same
/// problem must render as one line with a summed `count`, never as one
/// duplicate line per occurrence.
///
/// In non-verbose mode, path/chain are never printed (see
/// [`render_diagnostic`]), so entries are collapsed purely by `category`,
/// summing every occurrence's count into a single line.
///
/// In verbose mode, path/chain carry distinct per-occurrence detail (e.g.
/// which file was skipped), so entries are collapsed by the full
/// `(category, verbose_path, verbose_chain)` shape instead, preserving one
/// line per distinct detail while still merging exact duplicates.
fn aggregate_diagnostics(errors: &[Diagnostic], verbose: bool) -> Vec<Diagnostic> {
    if verbose {
        aggregate_by(errors, |e| {
            (e.category, e.verbose_path.clone(), e.verbose_chain.clone())
        })
    } else {
        aggregate_by(errors, |e| (e.category, None::<PathBuf>, None::<String>))
    }
}

fn aggregate_by<K, F>(errors: &[Diagnostic], key_fn: F) -> Vec<Diagnostic>
where
    K: Eq + std::hash::Hash + Clone,
    F: Fn(&Diagnostic) -> K,
{
    let mut order: Vec<K> = Vec::new();
    let mut totals: HashMap<K, (usize, &Diagnostic)> = HashMap::new();
    for error in errors {
        let key = key_fn(error);
        match totals.get_mut(&key) {
            Some((count, _)) => *count += error.count,
            None => {
                order.push(key.clone());
                totals.insert(key, (error.count, error));
            }
        }
    }
    order
        .into_iter()
        .map(|key| {
            let (count, template) = &totals[&key];
            Diagnostic {
                category: template.category,
                count: *count,
                verbose_path: template.verbose_path.clone(),
                verbose_chain: template.verbose_chain.clone(),
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
fn discovery_exit(
    records: &[CandidateRecord],
    state: &DiscoveryState,
    no_agents_selected: bool,
) -> i32 {
    if !no_agents_selected
        && records.is_empty()
        && state.successful_integrations.load(Ordering::SeqCst) == 0
    {
        EXIT_ERROR
    } else {
        EXIT_OK
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{ActivityStatus, RiskStatus, WorkspaceEvidence};
    use std::ffi::OsString;
    #[test]
    fn empty_list_has_human_readable_fallback() {
        assert_eq!(empty_list_message(&[]), Some("No Sessions found in Scope."));
    }

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
            updated_at: Some(crate::session::UpdateTime {
                at: std::time::SystemTime::now(),
                source: crate::session::UpdateTimeSource::Native,
            }),
            workspace: WorkspaceEvidence::Recorded {
                workspace: "/workspace".into(),
                historical_git_identity: None,
            },
            support: SupportStatus::Supported,
            activity: ActivityStatus::Unknown,
            risk: RiskStatus::Normal,
        };
        let item = picker_candidate(CandidateKey(1), &session);
        assert!(item.display.starts_with("0m         omp[work]"));
        assert!(item.preview.contains("native timestamp"));
        assert!(item.display.contains("no-branch"));
        assert!(
            !item.display.contains('+'),
            "title and branch are separate columns, not glued with '+'"
        );
        assert!(!item.search_text.contains("/workspace"));
    }

    #[test]
    fn resume_selected_reports_e3003_for_every_non_supported_status() {
        // errors-unified-catalog-e3003-unsupported-resume: no test previously
        // drove `resume_selected` itself (only `Report`'s own Display/exit
        // code were tested, at src/errors.rs). This proves each of the three
        // non-Supported statuses -- and a Supported session missing its
        // ResumeSpec/LaunchEvidence -- actually reaches the E3003 branch
        // instead of silently falling through to `launch::exec`.
        fn session_with(support: SupportStatus) -> Session {
            Session {
                key: crate::session::SessionKey {
                    agent: "codex".into(),
                    effective_root: "/r".into(),
                    profile: None,
                    native_locator: "/t".into(),
                },
                resumable_id: "id".into(),
                title: Some("title".into()),
                updated_at: None,
                workspace: WorkspaceEvidence::Recorded {
                    workspace: "/workspace".into(),
                    historical_git_identity: None,
                },
                support,
                activity: ActivityStatus::Unknown,
                risk: RiskStatus::Normal,
            }
        }
        let options = effective_options(
            &default_cli(),
            Config::default(),
            settings::Settings::default(),
        )
        .unwrap();
        for support in [
            SupportStatus::Unsupported,
            SupportStatus::DiscoverOnly,
            SupportStatus::Unavailable,
        ] {
            let record = CandidateRecord {
                session: session_with(support),
                spec: None,
                evidence: None,
            };
            assert_eq!(
                resume_selected(record, &options),
                crate::errors::E3003.exit_code,
                "{support:?} must exit with E3003's code"
            );
        }
        // Supported but missing spec/evidence: the second E3003 branch.
        let record = CandidateRecord {
            session: session_with(SupportStatus::Supported),
            spec: None,
            evidence: None,
        };
        assert_eq!(
            resume_selected(record, &options),
            crate::errors::E3003.exit_code,
            "Supported without spec/evidence must also exit with E3003's code"
        );
    }

    /// `docs/product-design.md` Â§3: "Title allocation is at most 60
    /// columns on a wide terminal and at least 16 columns in the compact
    /// layout". `title_column_width` must stay within `[16, 60]` for every
    /// plausible terminal width, and fall back to a stable default with no
    /// controlling terminal (the common case for `--list`/`--json` in tests,
    /// CI, and redirected/piped invocations).
    #[test]
    fn title_column_width_stays_within_documented_bounds() {
        let width = title_column_width();
        assert!(
            (TITLE_WIDTH_MIN..=TITLE_WIDTH_MAX).contains(&width),
            "width {width} outside [{TITLE_WIDTH_MIN}, {TITLE_WIDTH_MAX}]"
        );
        // No controlling terminal in the test harness: falls back to the
        // fixed default rather than clamping to an arbitrary bound.
        assert_eq!(width, TITLE_WIDTH_DEFAULT);
    }
    #[test]
    fn non_verbose_diagnostics_collapse_by_category_summing_counts() {
        // Reproduces the real-world spam of N distinct-path diagnostics in
        // the same category (e.g. many `claude_no_session_id` skips) each
        // rendering as their own duplicate line instead of one aggregated
        // count, per `Diagnostic`'s "redacted category, count, ..." contract.
        let errors = vec![
            Diagnostic {
                category: "claude_no_session_id",
                count: 1,
                verbose_path: Some(PathBuf::from("/a/one.jsonl")),
                verbose_chain: Some("no embedded sessionId and no cwd; skipped".into()),
            },
            Diagnostic {
                category: "claude_no_session_id",
                count: 1,
                verbose_path: Some(PathBuf::from("/a/two.jsonl")),
                verbose_chain: Some("no embedded sessionId and no cwd; skipped".into()),
            },
            Diagnostic {
                category: "claude_no_session_id",
                count: 1,
                verbose_path: Some(PathBuf::from("/a/three.jsonl")),
                verbose_chain: Some("no embedded sessionId and no cwd; skipped".into()),
            },
            Diagnostic {
                category: "pi_skipped",
                count: 2,
                verbose_path: None,
                verbose_chain: None,
            },
        ];

        let collapsed = aggregate_diagnostics(&errors, false);
        assert_eq!(collapsed.len(), 2, "one line per distinct category");
        let claude = collapsed
            .iter()
            .find(|d| d.category == "claude_no_session_id")
            .expect("claude category present");
        assert_eq!(claude.count, 3, "three occurrences summed into one count");
        let pi = collapsed
            .iter()
            .find(|d| d.category == "pi_skipped")
            .expect("pi category present");
        assert_eq!(pi.count, 2);

        let rendered: Vec<String> = collapsed
            .iter()
            .map(|d| render_diagnostic(d, false))
            .collect();
        assert!(rendered.contains(&"resume: claude_no_session_id: 3".to_string()));
        assert!(rendered.contains(&"resume: pi_skipped: 2".to_string()));
    }

    #[test]
    fn verbose_diagnostics_keep_distinct_paths_but_merge_exact_duplicates() {
        let errors = vec![
            Diagnostic {
                category: "claude_no_session_id",
                count: 1,
                verbose_path: Some(PathBuf::from("/a/one.jsonl")),
                verbose_chain: Some("skipped".into()),
            },
            Diagnostic {
                category: "claude_no_session_id",
                count: 1,
                verbose_path: Some(PathBuf::from("/a/two.jsonl")),
                verbose_chain: Some("skipped".into()),
            },
            // Exact duplicate of the first entry (same category/path/chain).
            Diagnostic {
                category: "claude_no_session_id",
                count: 1,
                verbose_path: Some(PathBuf::from("/a/one.jsonl")),
                verbose_chain: Some("skipped".into()),
            },
        ];

        let collapsed = aggregate_diagnostics(&errors, true);
        assert_eq!(
            collapsed.len(),
            2,
            "distinct paths remain separate lines in verbose mode"
        );
        let one = collapsed
            .iter()
            .find(|d| d.verbose_path.as_deref() == Some(std::path::Path::new("/a/one.jsonl")))
            .expect("path one present");
        assert_eq!(one.count, 2, "exact duplicates merged and summed");
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
    fn git_scope_failure_becomes_a_visible_diagnostic() {
        let warning = scope_warning_diagnostic(Some("git executable unavailable".into()))
            .expect("Git failure should be surfaced");

        assert_eq!(warning.category, "git_scope_discovery_failed");
        assert_eq!(warning.count, 1);
        assert_eq!(
            render_diagnostic(&warning, false),
            "resume: git_scope_discovery_failed: 1"
        );
        assert!(
            render_diagnostic(&warning, true).contains("git executable unavailable"),
            "verbose diagnostics should explain the Git failure"
        );
    }

    #[test]
    fn unknown_agent_is_usage_error() {
        let cli = Cli {
            directory: None,
            up: None,
            down: None,
            all_worktrees: false,
            agent: vec![OsString::from("bad")],
            since: None,
            list: false,
            json: false,
            verbose: false,
            config: None,
            confirm_always: false,
            no_confirm: false,
            man: false,
            command: None,
        };
        assert!(effective_options(&cli, Config::default(), settings::Settings::default()).is_err());
    }

    fn default_cli() -> Cli {
        Cli {
            directory: None,
            up: None,
            down: None,
            all_worktrees: false,
            agent: Vec::new(),
            since: None,
            list: false,
            json: false,
            verbose: false,
            config: None,
            confirm_always: false,
            no_confirm: false,
            man: false,
            command: None,
        }
    }

    /// config-confirm-always-field / config-verbose-field: `effective_options`
    /// must OR the CLI flag with the configured value, not just read the CLI
    /// flag. Neither field previously had an isolated unit test (E2E gap
    /// flagged by the feature-inventory audit); both were only verified
    /// manually against a real PTY session.
    #[test]
    fn config_confirm_always_and_verbose_flow_into_effective_options() {
        let cli = default_cli();
        let config = Config {
            confirm_always: Some(true),
            verbose: Some(true),
            ..Config::default()
        };
        let options = effective_options(&cli, config, settings::Settings::default()).unwrap();
        assert!(
            options.confirm_always,
            "config.confirm_always=true must set EffectiveOptions.confirm_always even with no CLI flag"
        );
        assert!(
            options.verbose,
            "config.verbose=true must set EffectiveOptions.verbose even with no CLI flag"
        );

        // Absent from config and CLI, both default to false.
        let options =
            effective_options(&cli, Config::default(), settings::Settings::default()).unwrap();
        assert!(!options.confirm_always);
        assert!(!options.verbose);
    }

    /// cli-agent-case-insensitive: `-a PI`/`-a Codex` must behave identically
    /// to lowercase. Previously only verified by reading the
    /// `.to_ascii_lowercase()` call site, with no automated assertion.
    #[test]
    fn agent_flag_is_case_insensitive() {
        let mut cli = default_cli();
        cli.agent = vec![OsString::from("PI"), OsString::from("Codex")];
        let options =
            effective_options(&cli, Config::default(), settings::Settings::default()).unwrap();
        assert_eq!(options.agents, vec!["pi".to_string(), "codex".to_string()]);
    }
}
