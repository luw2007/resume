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
use std::sync::Arc;
use std::time::SystemTime;

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

/// Internal control-flow result for the live streaming session: either a
/// terminal outcome to hand back to the caller, or a request (Alt+P) to
/// switch into the paginated view once discovery settles.
enum LiveExit {
    Terminal(PickerOutcome),
    Paginate,
}

fn classify_live(outcome: Option<SkimOutput>) -> LiveExit {
    if let Some(o) = &outcome {
        if !o.is_abort && matches!(o.final_key, SkimKey::Alt('p')) {
            return LiveExit::Paginate;
        }
    }
    LiveExit::Terminal(classify(outcome))
}

/// Internal control-flow result for one paginated page: either a terminal
/// outcome, or a request to move to the older/newer page.
enum PageExit {
    Terminal(PickerOutcome),
    Older,
    Newer,
}

fn classify_page(outcome: Option<SkimOutput>) -> PageExit {
    if let Some(o) = &outcome {
        if !o.is_abort {
            match o.final_key {
                SkimKey::Alt('p') => return PageExit::Older,
                SkimKey::Alt('n') => return PageExit::Newer,
                _ => {}
            }
        }
    }
    PageExit::Terminal(classify(outcome))
}

/// Immutable production candidate. Display text is never identity or launch state.
#[derive(Clone, Debug)]
pub struct PickerCandidate {
    pub key: CandidateKey,
    pub display: String,
    pub search_text: String,
    pub preview: String,
    /// Ascending ordering key for the paginated view (oldest first, most
    /// recently active last — the bottom of the final page). Mirrors
    /// `session::compare_sessions`, reversed.
    pub rank: Option<SystemTime>,
}

/// Production bounded streaming picker, preserving the Step 2 interaction
/// contract. Once the live stream ends in an Alt+P request, transparently
/// hands off to [`run_paginated_picker`] over every candidate seen so far
/// (including any still arriving from discovery).
pub fn run_production_picker(
    candidates: std::sync::mpsc::Receiver<PickerCandidate>,
    preview_mode: PreviewMode,
    preview_position: PreviewPosition,
) -> PickerOutcome {
    if let Err(reason) = preflight() {
        return PickerOutcome::PreflightFailed(reason);
    }
    let store = Arc::new(PreviewStore::default());
    // The accepted dual-section fallback always exposes normalized and raw in Preview.
    store
        .show_raw
        .store(true, std::sync::atomic::Ordering::Relaxed);
    let (tx, rx): (SkimItemSender, SkimItemReceiver) = bounded(crate::runtime::CHANNEL_CAPACITY);
    let producer_store = store.clone();
    let producer = std::thread::spawn(move || {
        let mut seen = Vec::new();
        while let Ok(candidate) = candidates.recv() {
            producer_store.insert(candidate.key.clone(), candidate.preview.clone());
            // Cloned rather than moved: `candidate` is also buffered below so
            // it survives for a possible later pagination hand-off.
            let item = Arc::new(SpikeItem {
                key: candidate.key.clone(),
                display: candidate.display.clone(),
                search_text: candidate.search_text.clone(),
                preview_store: producer_store.clone(),
            }) as Arc<dyn SkimItem>;
            let forwarded = tx.send(item).is_ok();
            seen.push(candidate);
            if !forwarded {
                return (seen, Some(candidates));
            }
        }
        (seen, None)
    });
    let options = build_production_options(preview_mode, preview_position);
    let result = run_skim_with_options(&options, rx);
    match classify_live(result) {
        // Deliberately not joined here: the producer thread blocks on
        // `candidates.recv()`, which only unblocks once every discovery-worker
        // sender upstream (in `app::run_interactive`) drops — independent of
        // whether Skim already returned a selection. Joining would make
        // Resume wait for the slowest discovery worker instead of the user's
        // keystroke. The caller already cancels discovery and reaps its
        // workers on a budget after this function returns, which drops the
        // remaining senders and lets this thread finish on its own.
        LiveExit::Terminal(outcome) => outcome,
        // Alt+P is an explicit user request to see the rest of the results,
        // so — unlike the path above — we do join and drain the remainder,
        // effectively waiting for discovery to finish.
        LiveExit::Paginate => {
            let (mut seen, remaining) = producer.join().unwrap_or_default();
            if let Some(remaining) = remaining {
                while let Ok(candidate) = remaining.recv() {
                    store.insert(candidate.key.clone(), candidate.preview.clone());
                    seen.push(candidate);
                }
            }
            seen.sort_by(|a, b| a.rank.cmp(&b.rank).then_with(|| a.key.0.cmp(&b.key.0)));
            run_paginated_picker(&store, &seen, preview_mode, preview_position)
        }
    }
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

fn build_production_options(mode: PreviewMode, position: PreviewPosition) -> SkimOptions {
    let (position, visibility) = preview_layout(mode, position);
    SkimOptionsBuilder::default()
        .height(String::from("100%"))
        .multi(false)
        .header(Some(String::from("UPDATED  AGENT[PROFILE]  TITLE  BRANCH")))
        .preview(Some(String::new()))
        .preview_window(format!("{position}:60%{visibility}"))
        .bind(vec![
            String::from("ctrl-o:toggle-preview"),
            String::from("ctrl-r:ignore"),
            String::from("enter:accept"),
            String::from("esc:abort"),
            // Alt+P requests the paginated view (see `run_production_picker`).
            String::from("alt-p:accept"),
        ])
        .build()
        .expect("hardcoded production skim options are valid")
}

/// Run the paginated view once the live stream ends with a pagination
/// request: `candidates` is the full, already-sorted (ascending) buffer.
/// Starts on the last page (most recently active sessions) and relaunches a
/// fresh, small Skim instance for every page turn (Alt+P older / Alt+N
/// newer), since Skim has no API to reorder or replace an already-open list.
fn run_paginated_picker(
    store: &Arc<PreviewStore>,
    candidates: &[PickerCandidate],
    preview_mode: PreviewMode,
    preview_position: PreviewPosition,
) -> PickerOutcome {
    if candidates.is_empty() {
        return PickerOutcome::Cancelled;
    }
    let total_pages = candidates.len().div_ceil(PAGE_SIZE);
    let mut page_index = total_pages - 1;
    loop {
        let start = page_index * PAGE_SIZE;
        let end = (start + PAGE_SIZE).min(candidates.len());
        match run_single_page(
            store,
            &candidates[start..end],
            page_index,
            total_pages,
            preview_mode,
            preview_position,
        ) {
            PageExit::Older => page_index -= 1,
            PageExit::Newer => page_index += 1,
            PageExit::Terminal(outcome) => return outcome,
        }
    }
}

fn run_single_page(
    store: &Arc<PreviewStore>,
    page: &[PickerCandidate],
    page_index: usize,
    total_pages: usize,
    preview_mode: PreviewMode,
    preview_position: PreviewPosition,
) -> PageExit {
    let (tx, rx): (SkimItemSender, SkimItemReceiver) = bounded(page.len().max(1));
    for candidate in page {
        let item = Arc::new(SpikeItem {
            key: candidate.key.clone(),
            display: candidate.display.clone(),
            search_text: candidate.search_text.clone(),
            preview_store: store.clone(),
        }) as Arc<dyn SkimItem>;
        let _ = tx.send(item);
    }
    drop(tx);
    let options = build_paginated_options(page_index, total_pages, preview_mode, preview_position);
    classify_page(run_skim_with_options(&options, rx))
}

fn build_paginated_options(
    page_index: usize,
    total_pages: usize,
    mode: PreviewMode,
    position: PreviewPosition,
) -> SkimOptions {
    let (position, visibility) = preview_layout(mode, position);
    let mut binds = vec![
        String::from("ctrl-o:toggle-preview"),
        String::from("ctrl-r:ignore"),
        String::from("enter:accept"),
        String::from("esc:abort"),
    ];
    if page_index > 0 {
        binds.push(String::from("alt-p:accept")); // older page
    }
    if page_index + 1 < total_pages {
        binds.push(String::from("alt-n:accept")); // newer page
    }
    SkimOptionsBuilder::default()
        .height(String::from("100%"))
        .multi(false)
        .header(Some(format!(
            "PAGE {}/{} (alt-p older / alt-n newer)  UPDATED  AGENT[PROFILE]  TITLE  BRANCH",
            page_index + 1,
            total_pages
        )))
        .preview(Some(String::new()))
        .preview_window(format!("{position}:60%{visibility}"))
        .bind(binds)
        .build()
        .expect("hardcoded paginated skim options are valid")
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
    fn production_picker_uses_session_column_header() {
        let options = build_production_options(PreviewMode::Hidden, PreviewPosition::Auto);
        assert_eq!(
            options.header.as_deref(),
            Some("UPDATED  AGENT[PROFILE]  TITLE  BRANCH")
        );
    }
}
