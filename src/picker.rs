//! Step 2 — Embedded Skim feasibility gate.
//!
//! This module is a **throwaway-capable spike** whose only job is to prove
//! (or disprove) that Skim's public library API meets `resume`'s essential
//! interaction model without a fork or a second TUI. It deliberately does not
//! build the Active modal, adapter logic, or polished row layout.
//!
//! Every interaction codified here is backed by an automated PTY test in
//! `tests/picker_spike.rs`. The exit semantics, pre-flight checks, and
//! terminal-restoration guarantees are the decision-gate evidence.
//!
//! ## Proven interactions (see tests for the exact behaviors)
//!
//! | Requirement | Skim mechanism | Status |
//! |---|---|---|
//! | Streamed candidates from a bounded channel | `crossbeam::channel::bounded` + `SkimItemReceiver` | ✅ |
//! | `display` / `text` / `output` separation | custom `SkimItem` impl | ✅ |
//! | In-memory preview with native scrolling | per-item `preview()` returning `ItemPreview::Text` | ✅ |
//! | Preview hidden by default, `Ctrl+O` toggle | `preview_window = "right:60%:hidden"` + `bind "ctrl-o:toggle-preview"` | ✅ |
//! | `Ctrl+R` normalized/raw switch | dual-section `ItemPreview::Text` always rendered; `ctrl-r:ignore` (reload is unsafe: re-runs default `find`) | ✅ (dual-section fallback; reload rejected as unsafe) |
//! | Opaque-identity selection stability while streaming | `Arc<dyn SkimItem>` identity preserved; Skim sorts matched results but never swaps which item a key points at | ✅ (documented) |
//! | Terminal restoration on every exit path | tuikit `clear_on_exit`; `catch_unwind` around `run_with` for the panic path | ✅ |
//! | `/dev/tty` operation under redirected stdin | Skim opens `/dev/tty` directly via `get_tty()`, independent of fd 0 | ✅ (inherent) |
//! | No control terminal / tiny terminal fail before start | `preflight` checks before `run_with` | ✅ |
//! | ANSI/OSC/control-sequence safe content | `sanitize_for_display` + `ItemPreview::Text` (never `Command`) | ✅ |

use std::borrow::Cow;
use std::collections::HashMap;
use std::panic::{self, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use crate::config::{PreviewMode, PreviewPosition};

use skim::prelude::{
    AnsiString, DisplayContext, ItemPreview, PreviewContext, Skim, SkimItem, SkimItemReceiver,
    SkimItemSender, SkimOptions, SkimOptionsBuilder, SkimOutput, bounded,
};
use skim::tuikit::key::Key as SkimKey;

/// Minimum terminal geometry before the picker is allowed to start.
/// Anything smaller is treated as an unsupported terminal, matching the
/// Step 2 / Step 10 requirements ("terminal smaller than 60×10 fail before
/// picker start").
pub const MIN_TERM_WIDTH: usize = 60;
pub const MIN_TERM_HEIGHT: usize = 10;

/// Maximum candidates rendered per page once the live view switches into
/// paginated mode (Alt+P after the live stream). Every discovered session is
/// retained in memory; only the per-page render is capped.
pub const PAGE_SIZE: usize = 50;

/// An opaque, content-independent identity for a spike candidate.
///
/// Invariant under test: the value returned from `SkimOutput::selected_items`
/// downcasts back to the *same* `CandidateKey` regardless of how many items
/// arrived after it, or how Skim reordered the visible rows. A rendered row is
/// never identity.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct CandidateKey(pub u64);

/// A `SkimItem` that strictly separates the three Skim presentation channels:
///
/// - `display`: the safe, sanitized text shown in the list.
/// - `text`: the searchable text (kept identical to display here; in production
///   it carries metadata that is searchable but not necessarily displayed).
/// - `output`: deliberately **not** the launch state — the opaque key only.
/// - `preview`: looked up from an in-memory map, never executed as a command.
///
/// The `key` field is what makes selection stable: it is an opaque identity
/// that survives Skim's internal sorting of the *visible* list.
#[derive(Clone, Debug)]
pub struct SpikeItem {
    pub key: CandidateKey,
    pub display: String,
    pub search_text: String,
    /// Pointer to the shared in-memory preview store + current mode, so the
    /// preview is resolved at render time without a shell command.
    pub preview_store: Arc<PreviewStore>,
}

impl SkimItem for SpikeItem {
    fn text(&self) -> Cow<'_, str> {
        Cow::Borrowed(&self.search_text)
    }

    fn display<'a>(&'a self, context: DisplayContext<'a>) -> AnsiString<'a> {
        // The sanitized display string is rendered as-is; we never replay the
        // caller's ANSI. Skim's highlight context is ignored here because the
        // sanitized string has different byte offsets than the raw input; in
        // production the sanitizer preserves offsets so highlighting can be
        // applied. For the spike, correctness of sanitization outranks
        // in-band highlighting.
        let _ = context;
        AnsiString::new_string(sanitize_for_display(&self.display), vec![])
    }

    fn preview(&self, _context: PreviewContext) -> ItemPreview {
        // In-memory lookup. We deliberately return `ItemPreview::Text`, never
        // `ItemPreview::Command`, so no candidate or preview content is ever
        // executed by a shell. The dual-section fallback renders both the
        // normalized and raw forms in one preview, satisfying the accepted
        // Ctrl+R fallback without rebuilding the picker.
        ItemPreview::Text(self.preview_store.render(&self.key))
    }

    fn output(&self) -> Cow<'_, str> {
        // Opaque output: the key index only. Never a path, never launch state.
        Cow::Owned(format!("key:{}", self.key.0))
    }
}

/// In-memory preview store keyed by opaque identity. In production this is the
/// 64 MiB LRU cache from Step 3; here it is a plain map plus a render mode.
#[derive(Debug, Default)]
pub struct PreviewStore {
    /// Guarded by the single-producer-before-render discipline in the streamed
    /// path; for the synchronous path it is populated before `run_with`.
    pub entries: std::sync::Mutex<HashMap<CandidateKey, String>>,
    /// When true, the preview shows the raw (still terminal-safe) text in
    /// addition to the normalized form. Toggled by Ctrl+R via `reload`.
    pub show_raw: std::sync::atomic::AtomicBool,
}

impl PreviewStore {
    pub fn render(&self, key: &CandidateKey) -> String {
        let raw = self
            .entries
            .lock()
            .map(|m| m.get(key).cloned().unwrap_or_default())
            .unwrap_or_default();
        let normalized = fold_whitespace(&sanitize_for_display(&raw));
        if self.show_raw.load(std::sync::atomic::Ordering::Relaxed) {
            format!(
                "# normalized (terminal-safe)\n{normalized}\n\n# raw (still terminal-safe, unfiltered)\n{raw_safe}",
                raw_safe = sanitize_for_display(&raw)
            )
        } else {
            format!("# normalized\n{normalized}")
        }
    }

    fn insert(&self, key: CandidateKey, value: String) {
        if let Ok(mut m) = self.entries.lock() {
            m.insert(key, value);
        }
    }
}

/// Outcome of a picker run, carrying the opaque key of the chosen item (if any)
/// and the classified exit reason.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PickerOutcome {
    /// Normal selection: opaque key of the chosen item.
    Selected(CandidateKey),
    /// Esc / empty input / zero results — clean, user-initiated cancel.
    Cancelled,
    /// Ctrl+C interrupt.
    Interrupted,
    /// Pre-flight failed before the picker started (no TTY, tiny terminal).
    PreflightFailed(String),
    /// Skim itself returned no output (internal error / panic caught).
    InternalError(String),
}

/// Run the spike picker synchronously: all candidates are known up front.
///
/// `force_raw` overrides the normalized/raw preview mode before launch; the
/// PTY tests use it to prove the dual-section preview path without depending
/// on keybinding timing.
pub fn run_picker<I>(candidates: I, force_raw: bool) -> PickerOutcome
where
    I: IntoIterator<Item = (CandidateKey, String, String)>,
{
    if let Err(reason) = preflight() {
        return PickerOutcome::PreflightFailed(reason);
    }

    let preview_store = Arc::new(PreviewStore::default());
    if force_raw {
        preview_store
            .show_raw
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }
    let items: Vec<Arc<SpikeItem>> = candidates
        .into_iter()
        .map(|(key, display, preview)| {
            preview_store.insert(key.clone(), preview);
            let search_text = display.clone();
            Arc::new(SpikeItem {
                key: key.clone(),
                display,
                search_text,
                preview_store: preview_store.clone(),
            })
        })
        .collect();

    let outcome = run_skim_panic_safe_synchronous(&items);
    classify(outcome)
}

/// Bounded streaming variant: candidates arrive on a bounded crossbeam channel
/// while the picker is already open. This is the shape production uses (one
/// discovery worker per integration, backpressure via the bounded channel).
pub fn run_picker_streamed<I>(candidates: I, force_raw: bool) -> PickerOutcome
where
    I: IntoIterator<Item = (CandidateKey, String, String)> + Send + 'static,
    I::IntoIter: Send,
{
    if let Err(reason) = preflight() {
        return PickerOutcome::PreflightFailed(reason);
    }

    let preview_store = Arc::new(PreviewStore::default());
    if force_raw {
        preview_store
            .show_raw
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }

    const CHANNEL_BOUND: usize = 32;
    let (tx, rx): (SkimItemSender, SkimItemReceiver) = bounded(CHANNEL_BOUND);

    let producer_preview = preview_store.clone();
    let producer = std::thread::spawn(move || {
        for (key, display, preview) in candidates {
            // Bounded send = cooperative backpressure. If the picker exits
            // early, the receiver is dropped and `send` fails; we stop.
            let item = Arc::new(SpikeItem {
                key: key.clone(),
                display: display.clone(),
                search_text: display,
                preview_store: producer_preview.clone(),
            }) as Arc<dyn SkimItem>;
            producer_preview.insert(key, preview);
            if tx.send(item).is_err() {
                break; // picker closed the channel
            }
        }
        // drop(tx) signals "no more items" to Skim's reader.
    });

    let outcome = run_skim_panic_safe_streaming(rx);
    let _ = producer.join();
    classify(outcome)
}

/// Build the SkimOptions that codify the proven interaction model.
fn build_options() -> SkimOptions {
    SkimOptionsBuilder::default()
        .height(String::from("100%"))
        .multi(false)
        // Preview is enabled but **hidden by default**. The `:hidden` suffix
        // means the window is invisible until `toggle-preview` runs.
        .preview(Some(String::new()))
        .preview_window(String::from("right:60%:hidden"))
        .bind(vec![
            // Ctrl+O toggles preview visibility (native Skim action).
            String::from("ctrl-o:toggle-preview"),
            // Ctrl+R is deliberately bound to a no-op rather than `reload`.
            //
            // FINDING (decision-gate evidence): Skim's `reload` action with no
            // argument re-runs the *default command* (`find` on the cwd) even
            // when items were supplied via a channel, which would list real
            // files from the working directory into the picker. That is
            // unacceptable for a launcher fed by integration workers. The
            // accepted ADR fallback is the dual-section preview, which always
            // renders both normalized and raw text in the preview window — so
            // there is no need to rebuild the picker or re-run any command.
            // We bind Ctrl+R to `ignore` so the key is accepted harmlessly
            // and the dual-section preview remains the single source of truth.
            String::from("ctrl-r:ignore"),
            // Explicit accept/abort for deterministic PTY tests.
            String::from("enter:accept"),
            String::from("esc:abort"),
        ])
        .build()
        .expect("hardcoded skim options are valid")
}

fn run_skim_panic_safe_synchronous(items: &[Arc<SpikeItem>]) -> Option<SkimOutput> {
    let (tx, rx): (SkimItemSender, SkimItemReceiver) = bounded(items.len().max(1));
    for item in items {
        let _ = tx.send(item.clone() as Arc<dyn SkimItem>);
    }
    drop(tx);
    run_skim_panic_safe(rx)
}

fn run_skim_panic_safe_streaming(rx: SkimItemReceiver) -> Option<SkimOutput> {
    run_skim_panic_safe(rx)
}

fn run_skim_panic_safe(rx: SkimItemReceiver) -> Option<SkimOutput> {
    run_skim_with_options(&build_options(), rx)
}

/// Run Skim behind a panic guard, shared by the spike, production, and
/// paginated call sites. `Term::with_options` panics if the TUI cannot init
/// (e.g. /dev/tty missing despite preflight, or a panic inside rendering);
/// tuikit's `Term::Drop` still restores the terminal during the unwind, so we
/// only need to signal the caller that the run failed. A real `SkimOutput`
/// cannot be fabricated on panic (the `Event` type is private), so `None`
/// maps to `InternalError` in `classify`.
fn run_skim_with_options(options: &SkimOptions, rx: SkimItemReceiver) -> Option<SkimOutput> {
    panic::catch_unwind(AssertUnwindSafe(|| Skim::run_with(options, Some(rx))))
        .ok()
        .flatten()
}

fn classify(outcome: Option<SkimOutput>) -> PickerOutcome {
    let output = match outcome {
        Some(o) => o,
        None => return PickerOutcome::InternalError("skim returned no output".into()),
    };

    if output.is_abort {
        if is_interrupt(&output.final_key) {
            return PickerOutcome::Interrupted;
        }
        return PickerOutcome::Cancelled;
    }

    if let Some(first) = output.selected_items.first() {
        if let Some(item) = first.as_any().downcast_ref::<SpikeItem>() {
            return PickerOutcome::Selected(item.key.clone());
        }
        return PickerOutcome::InternalError("selected item was not a SpikeItem".into());
    }

    PickerOutcome::Cancelled
}

#[allow(dead_code)]
struct OutputInfo<'a> {
    is_abort: bool,
    final_key: &'a SkimKey,
    first_selected: Option<&'a Arc<dyn SkimItem>>,
}

#[allow(dead_code)]
fn output_info(o: &SkimOutput) -> OutputInfo<'_> {
    OutputInfo {
        is_abort: o.is_abort,
        final_key: &o.final_key,
        first_selected: o.selected_items.first(),
    }
}

fn is_interrupt(key: &SkimKey) -> bool {
    matches!(key, SkimKey::Ctrl('c'))
}

/// Internal control-flow result for one rendered tab+page view: either a
/// terminal outcome, or a request to move to a different page or tab.
enum NavExit {
    Terminal(PickerOutcome),
    OlderPage,
    NewerPage,
    PrevTab,
    NextTab,
}

fn classify_nav(outcome: Option<SkimOutput>) -> NavExit {
    if let Some(o) = &outcome
        && !o.is_abort
    {
        match o.final_key {
            SkimKey::Alt('p') => return NavExit::OlderPage,
            SkimKey::Alt('n') => return NavExit::NewerPage,
            SkimKey::AltLeft | SkimKey::Left | SkimKey::BackTab => return NavExit::PrevTab,
            SkimKey::AltRight | SkimKey::Right | SkimKey::Tab => return NavExit::NextTab,
            _ => {}
        }
    }
    NavExit::Terminal(classify(outcome))
}

/// Immutable production candidate. Display text is never identity or launch state.
#[derive(Clone, Debug)]
pub struct PickerCandidate {
    pub key: CandidateKey,
    pub display: String,
    pub search_text: String,
    pub preview: String,
    /// Ascending ordering key (oldest first, most recently active last — the
    /// bottom of the final page). Mirrors `session::compare_sessions`, reversed.
    pub rank: Option<SystemTime>,
    /// Agent name, used to build the per-agent tabs (`Alt+Left`/`Alt+Right`).
    pub agent: String,
}

/// A discovery worker still running when the picker opens (see
/// `app::run_interactive`): its `label` is shown in the header until
/// `pending` clears. Generic over which agent it names — `picker` has no
/// agent-specific knowledge, only a name and a flag.
pub struct BackgroundAgent {
    pub label: String,
    pub pending: Arc<AtomicBool>,
}

/// Run the full production picker: an "All" tab plus one tab per distinct
/// agent present in `candidates`, each sorted ascending by `rank` and
/// paginated at [`PAGE_SIZE`]. Starts on the newest page of the "All" tab,
/// which is always filled before an older remainder page. `Alt+P`/`Alt+N`
/// move between older and newer pages of the current tab; `Alt+Left`/
/// `Alt+Right`, `Left`/`Right`, and `Tab`/`Shift+Tab` all switch tabs
/// (wrapping), resetting to that tab's newest page.
///
/// `candidates` is shared and may keep growing after this call starts: a
/// `background` agent (see [`BackgroundAgent`]) can still be discovering
/// when the picker opens (`app::run_interactive` uses this for Codex, whose
/// per-file JSONL parsing cost is not bounded the way the directory-pruned
/// agents' scans are). Each navigation re-reads the current snapshot, so a
/// tab a background agent contributes picks up its Sessions as soon as they
/// land — but never mid-render, only on the next page turn or tab switch.
/// The current tab is tracked by agent name, not index, so a tab list that
/// grows between renders (a background agent's first Session arriving)
/// never silently retargets the user onto the wrong tab.
pub fn run_tabbed_picker(
    candidates: Arc<Mutex<Vec<PickerCandidate>>>,
    preview_mode: PreviewMode,
    preview_position: PreviewPosition,
    background: Option<BackgroundAgent>,
) -> PickerOutcome {
    if let Err(reason) = preflight() {
        return PickerOutcome::PreflightFailed(reason);
    }
    let mut announced_wait = false;
    loop {
        if !candidates.lock().unwrap().is_empty() {
            break;
        }
        match &background {
            Some(bg) if bg.pending.load(Ordering::Relaxed) => {
                if !announced_wait {
                    eprintln!("resume: waiting for {} to finish scanning...", bg.label);
                    announced_wait = true;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            // Nothing else can ever produce a candidate: background is
            // absent, or it already finished with zero Sessions.
            _ => return PickerOutcome::Cancelled,
        }
    }

    let store = Arc::new(PreviewStore::default());
    // The accepted dual-section fallback always exposes normalized and raw in Preview.
    store.show_raw.store(true, Ordering::Relaxed);

    // `None` is the "All" tab; `Some(agent)` names a per-agent tab. Tracked
    // by name (not index) so tab-list growth between renders never
    // retargets the user at the wrong position.
    let mut current_tab: Option<String> = None;
    let mut page_index = 0usize; // zero is the newest page
    loop {
        let mut snapshot = candidates.lock().unwrap().clone();
        snapshot.sort_by(|a, b| a.rank.cmp(&b.rank).then_with(|| a.key.0.cmp(&b.key.0)));
        for candidate in &snapshot {
            store.insert(candidate.key.clone(), candidate.preview.clone());
        }

        // Tab 0 is "All"; tabs 1.. are each distinct agent, in first-seen order.
        let mut agent_tabs: Vec<&str> = Vec::new();
        for candidate in &snapshot {
            if !agent_tabs.contains(&candidate.agent.as_str()) {
                agent_tabs.push(&candidate.agent);
            }
        }
        let total_tabs = agent_tabs.len() + 1;
        let tab_index = current_tab
            .as_deref()
            .and_then(|name| agent_tabs.iter().position(|a| *a == name))
            .map_or(0, |i| i + 1);

        let tab_candidates: Vec<&PickerCandidate> = if tab_index == 0 {
            snapshot.iter().collect()
        } else {
            snapshot
                .iter()
                .filter(|c| c.agent == agent_tabs[tab_index - 1])
                .collect()
        };
        let total_pages = tab_candidates.len().div_ceil(PAGE_SIZE).max(1);
        page_index = page_index.min(total_pages - 1);
        let page = page_bounds(tab_candidates.len(), page_index);
        let pending_label = background
            .as_ref()
            .filter(|bg| bg.pending.load(Ordering::Relaxed))
            .map(|bg| bg.label.as_str());

        match run_single_view(SingleView {
            store: &store,
            candidates: &tab_candidates[page],
            tab_index,
            agent_tabs: &agent_tabs,
            page: PageInfo {
                index: page_index,
                total: total_pages,
                candidates: tab_candidates.len(),
            },
            preview_mode,
            preview_position,
            pending_label,
        }) {
            NavExit::OlderPage if page_index + 1 < total_pages => page_index += 1,
            NavExit::NewerPage if page_index > 0 => page_index -= 1,
            NavExit::OlderPage | NavExit::NewerPage => {}
            NavExit::PrevTab => {
                let new_index = (tab_index + total_tabs - 1) % total_tabs;
                current_tab = (new_index > 0).then(|| agent_tabs[new_index - 1].to_string());
                page_index = 0;
            }
            NavExit::NextTab => {
                let new_index = (tab_index + 1) % total_tabs;
                current_tab = (new_index > 0).then(|| agent_tabs[new_index - 1].to_string());
                page_index = 0;
            }
            NavExit::Terminal(outcome) => return outcome,
        }
    }
}

fn page_bounds(total_candidates: usize, page_index: usize) -> std::ops::Range<usize> {
    let end = total_candidates - page_index * PAGE_SIZE;
    let start = end.saturating_sub(PAGE_SIZE);
    start..end
}

fn preview_layout(mode: PreviewMode, position: PreviewPosition) -> (&'static str, &'static str) {
    let position = match position {
        PreviewPosition::Right => "right",
        PreviewPosition::Bottom => "down",
        PreviewPosition::Auto => {
            if tty_size().is_some_and(|(width, _)| width >= 100) {
                "right"
            } else {
                "down"
            }
        }
    };
    let visibility = if mode == PreviewMode::Hidden {
        ":hidden"
    } else {
        ""
    };
    (position, visibility)
}

struct PageInfo {
    index: usize,
    total: usize,
    candidates: usize,
}

struct SingleView<'a> {
    store: &'a Arc<PreviewStore>,
    candidates: &'a [&'a PickerCandidate],
    tab_index: usize,
    agent_tabs: &'a [&'a str],
    page: PageInfo,
    preview_mode: PreviewMode,
    preview_position: PreviewPosition,
    pending_label: Option<&'a str>,
}

fn run_single_view(view: SingleView<'_>) -> NavExit {
    let (tx, rx): (SkimItemSender, SkimItemReceiver) = bounded(view.candidates.len().max(1));
    for candidate in view.candidates {
        let item = Arc::new(SpikeItem {
            key: candidate.key.clone(),
            display: candidate.display.clone(),
            search_text: candidate.search_text.clone(),
            preview_store: view.store.clone(),
        }) as Arc<dyn SkimItem>;
        let _ = tx.send(item);
    }
    drop(tx);
    let options = build_tabbed_options(
        view.tab_index,
        view.agent_tabs,
        view.page,
        view.preview_mode,
        view.preview_position,
        view.pending_label,
    );
    classify_nav(run_skim_with_options(&options, rx))
}

fn build_tabbed_options(
    tab_index: usize,
    agent_tabs: &[&str],
    page: PageInfo,
    mode: PreviewMode,
    position: PreviewPosition,
    pending_label: Option<&str>,
) -> SkimOptions {
    let (position, visibility) = preview_layout(mode, position);
    let mut binds = vec![
        String::from("ctrl-o:toggle-preview"),
        String::from("ctrl-r:ignore"),
        String::from("enter:accept"),
        String::from("esc:abort"),
    ];
    if page.index + 1 < page.total {
        binds.push(String::from("alt-p:accept")); // older page
    }
    if page.index > 0 {
        binds.push(String::from("alt-n:accept")); // newer page
    }
    // "All" plus one tab per agent is always >= 2 tabs whenever there is any
    // data at all (run_tabbed_picker's wait loop only lets the render loop
    // start once there is at least one candidate), so tab switching is
    // unconditionally bound and wraps in the caller. Left/Right and Tab/
    // Shift-Tab are bound alongside Alt-Left/Alt-Right for the same move;
    // this sacrifices Skim's default arrow-key cursor movement inside the
    // typed filter query in exchange for one-key tab switching.
    binds.push(String::from("alt-left:accept")); // previous tab
    binds.push(String::from("alt-right:accept")); // next tab
    binds.push(String::from("left:accept")); // previous tab
    binds.push(String::from("right:accept")); // next tab
    binds.push(String::from("tab:accept")); // next tab
    binds.push(String::from("shift-tab:accept")); // previous tab

    let mut tabs = String::from(if tab_index == 0 { "[All]" } else { "All" });
    for (i, agent) in agent_tabs.iter().enumerate() {
        tabs.push(' ');
        if tab_index == i + 1 {
            tabs.push_str(&format!("[{agent}]"));
        } else {
            tabs.push_str(agent);
        }
    }
    let pending_note = pending_label
        .map(|label| format!("  ({label} still scanning)"))
        .unwrap_or_default();

    let older_count = page_bounds(page.candidates, page.index).start;
    let older_note = (older_count > 0).then(|| {
        format!(
            "  {older_count} older session{}: Alt-P",
            if older_count == 1 { "" } else { "s" }
        )
    });

    SkimOptionsBuilder::default()
        .height(String::from("100%"))
        .no_sort(true)
        .tac(true)
        .multi(false)
        .header(Some(format!(
            "{tabs}{pending_note}  PAGE {}/{}{older_note}  (alt-p/alt-n page, left/right or tab/shift-tab to switch)\nUPDATED  AGENT[PROFILE]  TITLE  BRANCH",
            page.index + 1,
            page.total,
            older_note = older_note.unwrap_or_default(),
        )))
        .preview(Some(String::new()))
        .preview_window(format!("{position}:60%{visibility}"))
        .bind(binds)
        .build()
        .expect("hardcoded tabbed skim options are valid")
}

// ---------------------------------------------------------------------------
// Pre-flight checks
// ---------------------------------------------------------------------------

/// Verify the terminal can host the picker before Skim takes over.
///
/// Returns `Err(reason)` if there is no controlling terminal or the terminal is
/// smaller than [`MIN_TERM_WIDTH`]×[`MIN_TERM_HEIGHT`]. This runs **before**
/// `Skim::run_with`, so the failure is reported cleanly instead of as a Skim
/// panic.
pub fn preflight() -> Result<(), String> {
    // 1. Controlling terminal: Skim opens /dev/tty directly (see
    //    skim-tuikit::raw::get_tty). If that device cannot be opened, the
    //    picker cannot run.
    if let Err(e) = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")
    {
        return Err(format!("no controlling terminal (/dev/tty: {e})"));
    }

    // 2. Terminal size via the same ioctl Skim/tuikit uses. We read it from
    //    /dev/tty to match Skim's own surface.
    let (w, h) = tty_size().ok_or_else(|| "could not determine terminal size".to_string())?;
    if w < MIN_TERM_WIDTH || h < MIN_TERM_HEIGHT {
        return Err(format!(
            "terminal too small: {w}x{h} (minimum {MIN_TERM_WIDTH}x{MIN_TERM_HEIGHT})"
        ));
    }
    Ok(())
}

#[cfg(unix)]
pub fn tty_size() -> Option<(usize, usize)> {
    use std::os::fd::AsRawFd;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")
        .ok()?;
    let fd = file.as_raw_fd();
    #[repr(C)]
    struct Winsize {
        ws_row: u16,
        ws_col: u16,
        ws_xpixel: u16,
        ws_ypixel: u16,
    }
    unsafe extern "C" {
        fn ioctl(fd: i32, request: u64, ...) -> i32;
    }
    #[cfg(target_os = "macos")]
    const TIOCGWINSZ: u64 = 0x40087468;
    #[cfg(target_os = "linux")]
    const TIOCGWINSZ: u64 = 0x5413;

    let mut ws = Winsize {
        ws_row: 0,
        ws_col: 0,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: ioctl with TIOCGWINSZ writes into a winsize struct of the
    // expected layout. fd is a valid open file descriptor to /dev/tty.
    let rc = unsafe { ioctl(fd, TIOCGWINSZ, &mut ws as *mut Winsize) };
    if rc != 0 || ws.ws_col == 0 || ws.ws_row == 0 {
        return None;
    }
    Some((ws.ws_col as usize, ws.ws_row as usize))
}

#[cfg(not(unix))]
pub fn tty_size() -> Option<(usize, usize)> {
    None
}

// ---------------------------------------------------------------------------
// Terminal-safe text handling
// ---------------------------------------------------------------------------

/// Sanitize untrusted text for display in the Skim list or preview.
///
/// Strips/neutralizes:
/// - ESC-led CSI / OSC sequences (ANSI colors, cursor moves, OSC 8 hyperlinks,
///   OSC 52 clipboard, title-setting sequences);
/// - other C0 controls except tab and newline (which the caller folds);
/// - C1 control bytes (0x80–0x9F) used as 8-bit ANSI introducers;
/// - common bidi-override characters (RLO/RLM/LRO etc.).
///
/// Returns a string that is safe to pass to `AnsiString::new_string` and
/// `ItemPreview::Text` (never `ItemPreview::Command` / `AnsiText` with raw
/// caller content). Ordinary Unicode is preserved.
///
/// This is the spike-local equivalent of the Step 3 `text` module; production
/// replaces it with the full normalization pipeline. It is intentionally
/// self-contained so Step 2 does not depend on Step 3.
pub fn sanitize_for_display(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == 0x1b {
            // ESC: consume the whole escape sequence.
            i = consume_escape(bytes, i + 1);
            continue;
        }
        // C0 controls: keep \t (0x09) and \n (0x0a); drop the rest.
        if b < 0x20 && b != 0x09 && b != 0x0a {
            i += 1;
            continue;
        }
        // Decode the next UTF-8 char to keep multi-byte sequences intact and
        // to strip bidi/zero-width/C1 controls at the codepoint level.
        let rest = &input[i..];
        match rest.chars().next() {
            Some(ch) => {
                let ch_len = ch.len_utf8();
                // C1 controls (U+0080–U+009F) in UTF-8 are 0xC2 0x80–0x9F.
                if (0x80..=0x9f).contains(&(ch as u32)) {
                    // U+009B is the C1 CSI introducer: consume the following
                    // CSI sequence just like ESC [.
                    if ch == '\u{9b}' {
                        let after = i + ch_len;
                        let consumed = consume_csi(bytes, after);
                        i = consumed;
                    } else {
                        // Other C1 string-terminator sequences (OSC/DCS/etc.)
                        if matches!(ch, '\u{9d}' | '\u{90}' | '\u{98}' | '\u{9e}' | '\u{9f}') {
                            let after = i + ch_len;
                            i = consume_string(bytes, after);
                        } else {
                            i += ch_len;
                        }
                    }
                    continue;
                }
                if is_bidi_or_invisible(ch) {
                    i += ch_len;
                } else {
                    out.push(ch);
                    i += ch_len;
                }
            }
            None => break,
        }
    }
    out
}

fn consume_escape(bytes: &[u8], i: usize) -> usize {
    // After ESC, possible introducers:
    //   '[' -> CSI
    //   ']' -> OSC (terminated by BEL or ST = ESC '\')
    //   'P'/'X'/'^'/'_' -> DCS/SOS/PM/APC (string, terminated by ST)
    //   '(' / ')' / '*' / '+' -> charset designation (one more byte)
    //   anything else -> a single-character escape (consume it)
    if i >= bytes.len() {
        return i;
    }
    match bytes[i] {
        b'[' => consume_csi(bytes, i + 1),
        b']' => consume_osc(bytes, i + 1),
        b'P' | b'X' | b'^' | b'_' => consume_string(bytes, i + 1),
        b'(' | b')' | b'*' | b'+' | b'=' | b'>' => i + 2,
        _ => i + 1,
    }
}

fn consume_csi(bytes: &[u8], mut i: usize) -> usize {
    // CSI = parameter bytes (0x30-0x3f) + intermediate bytes (0x20-0x2f)
    // + final byte (0x40-0x7e).
    while i < bytes.len() {
        let b = bytes[i];
        if (0x40..=0x7e).contains(&b) {
            return i + 1;
        }
        i += 1;
    }
    i
}

fn consume_osc(bytes: &[u8], mut i: usize) -> usize {
    // OSC is terminated by BEL (0x07) or ST (ESC '\').
    while i < bytes.len() {
        let b = bytes[i];
        if b == 0x07 {
            return i + 1;
        }
        if b == 0x1b && i + 1 < bytes.len() && bytes[i + 1] == b'\\' {
            return i + 2;
        }
        i += 1;
    }
    i
}

fn consume_string(bytes: &[u8], mut i: usize) -> usize {
    // DCS/SOS/PM/APC: terminated by ST (ESC '\') or BEL.
    while i < bytes.len() {
        let b = bytes[i];
        if b == 0x1b && i + 1 < bytes.len() && bytes[i + 1] == b'\\' {
            return i + 2;
        }
        if b == 0x07 {
            return i + 1;
        }
        i += 1;
    }
    i
}

/// Bidi overrides and invisible/format-control codepoints that must not appear
/// in rendered text because they can change the apparent meaning of a row.
fn is_bidi_or_invisible(ch: char) -> bool {
    matches!(
        ch,
        '\u{200e}' // LRM
        | '\u{200f}' // RLM
        | '\u{200b}' // ZWSP
        | '\u{200d}' // ZWJ
        | '\u{200c}' // ZWNJ
        | '\u{202a}' // LRE
        | '\u{202b}' // RLE
        | '\u{202c}' // PDF
        | '\u{202d}' // LRO
        | '\u{202e}' // RLO
        | '\u{2066}' // LRI
        | '\u{2067}' // RLI
        | '\u{2068}' // FSI
        | '\u{2069}' // PDI
        | '\u{2060}' // WJ
        | '\u{00ad}' // SHY
        | '\u{feff}' // ZWNBSP / BOM
    )
}

/// Fold newlines/tabs in list text to single spaces, for the normalized form.
pub fn fold_whitespace(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut prev_space = false;
    for ch in input.chars() {
        if ch == '\n' || ch == '\r' || ch == '\t' || ch == ' ' {
            if !prev_space {
                out.push(' ');
                prev_space = true;
            }
        } else {
            out.push(ch);
            prev_space = false;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use skim::Matches;
    use skim::tuikit::attr::Attr;

    #[test]
    fn sanitize_strips_ansi_color_and_csi() {
        let dirty = "\x1b[31mred\x1b[0m text";
        assert_eq!(sanitize_for_display(dirty), "red text");
    }

    #[test]
    fn sanitize_strips_osc8_hyperlink_and_osc52_clipboard() {
        let osc8 = "\x1b]8;;https://evil.example\x1b\\click\x1b]8;;\x1b\\";
        assert_eq!(sanitize_for_display(osc8), "click");
        let osc52 = "\x1b]52;c;AAA\x07";
        assert_eq!(sanitize_for_display(osc52), "");
    }

    #[test]
    fn sanitize_strips_title_and_cursor_sequences() {
        assert_eq!(sanitize_for_display("\x1b]0;title\x07"), "");
        assert_eq!(sanitize_for_display("\x1b[2J\x1b[Hclear"), "clear");
        assert_eq!(sanitize_for_display("a\x1b[5Cb"), "ab");
    }

    #[test]
    fn sanitize_strips_bidi_rlo_override() {
        // RLO can flip rendering of "file.txt" to look like a different name.
        let dirty = "file\u{202e}txt.exe";
        assert_eq!(sanitize_for_display(dirty), "filetxt.exe");
    }

    #[test]
    fn sanitize_strips_c1_single_byte_csi() {
        assert_eq!(sanitize_for_display("a\u{9b}31mb"), "ab");
    }

    #[test]
    fn sanitize_preserves_ordinary_unicode() {
        assert_eq!(
            sanitize_for_display("héllo 世界 日本語"),
            "héllo 世界 日本語"
        );
    }

    #[test]
    fn sanitize_strips_other_c0_controls() {
        assert_eq!(sanitize_for_display("a\x00b\x07c\x08d"), "abcd");
        // tab and newline preserved
        assert_eq!(sanitize_for_display("a\tb\nc"), "a\tb\nc");
    }

    #[test]
    fn fold_whitespace_collapses_runs() {
        assert_eq!(fold_whitespace("a\nb\tc  d"), "a b c d");
        assert_eq!(fold_whitespace("a\n\nb"), "a b");
    }

    #[test]
    fn spike_item_separates_display_text_output() {
        let store = Arc::new(PreviewStore::default());
        let item = SpikeItem {
            key: CandidateKey(7),
            display: "display \x1b[31mred\x1b[0m".into(),
            search_text: "searchable".into(),
            preview_store: store.clone(),
        };
        assert_eq!(&item.text().to_string(), "searchable");
        assert_eq!(&item.output().to_string(), "key:7");
        // display is sanitized: no ESC byte survives.
        let rendered = item.display(DisplayContext {
            text: "",
            score: 0,
            matches: Matches::None,
            container_width: 80,
            highlight_attr: Attr::default(),
        });
        let rendered_str: &str = rendered.stripped();
        assert!(!rendered_str.contains('\x1b'));
        assert_eq!(rendered_str, "display red");
    }

    #[test]
    fn preview_store_dual_section_when_raw_enabled() {
        let store = PreviewStore::default();
        store.insert(CandidateKey(1), "line\n\tone".into());
        let norm = store.render(&CandidateKey(1));
        assert!(norm.contains("normalized"));
        assert!(!norm.contains("raw"));
        store
            .show_raw
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let both = store.render(&CandidateKey(1));
        assert!(both.contains("normalized"));
        assert!(both.contains("raw"));
    }

    #[test]
    fn candidate_key_is_opaque_identity() {
        // The key type carries no path, agent, or launch state.
        let k = CandidateKey(42);
        assert_eq!(format!("{k:?}"), "CandidateKey(42)");
    }

    #[test]
    fn newest_page_is_full_before_older_remainder() {
        assert_eq!(page_bounds(PAGE_SIZE + 3, 0), 3..PAGE_SIZE + 3);
        assert_eq!(page_bounds(PAGE_SIZE + 3, 1), 0..3);
    }

    #[test]
    fn tabbed_picker_preserves_chronological_row_order() {
        let options = build_tabbed_options(
            0,
            &["omp"],
            PageInfo {
                index: 0,
                total: 1,
                candidates: 1,
            },
            PreviewMode::Hidden,
            PreviewPosition::Auto,
            None,
        );
        assert!(options.no_sort);
        assert!(options.tac);
    }

    #[test]
    fn tabbed_picker_header_advertises_older_remainder() {
        let options = build_tabbed_options(
            0,
            &["pi"],
            PageInfo {
                index: 0,
                total: 2,
                candidates: PAGE_SIZE + 3,
            },
            PreviewMode::Hidden,
            PreviewPosition::Auto,
            None,
        );
        assert!(
            options
                .header
                .as_deref()
                .is_some_and(|h| h.contains("PAGE 1/2  3 older sessions: Alt-P")),
            "header={:?}",
            options.header
        );
    }
    #[test]
    fn tabbed_picker_header_includes_session_columns() {
        let options = build_tabbed_options(
            0,
            &["pi"],
            PageInfo {
                index: 0,
                total: 1,
                candidates: 1,
            },
            PreviewMode::Hidden,
            PreviewPosition::Auto,
            None,
        );
        assert!(
            options
                .header
                .as_deref()
                .is_some_and(|h| h.contains("UPDATED  AGENT[PROFILE]  TITLE  BRANCH")),
            "header={:?}",
            options.header
        );
    }

    #[test]
    fn tabbed_picker_header_shows_pending_background_agent() {
        let without = build_tabbed_options(
            0,
            &["pi"],
            PageInfo {
                index: 0,
                total: 1,
                candidates: 1,
            },
            PreviewMode::Hidden,
            PreviewPosition::Auto,
            None,
        );
        assert!(!without.header.unwrap().contains("still scanning"));
        let with = build_tabbed_options(
            0,
            &["pi"],
            PageInfo {
                index: 0,
                total: 1,
                candidates: 1,
            },
            PreviewMode::Hidden,
            PreviewPosition::Auto,
            Some("codex"),
        );
        assert!(with.header.unwrap().contains("codex still scanning"));
    }
}
