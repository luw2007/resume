//! Step 2 spike entry point: a tiny CLI exercised by the PTY tests.
//!
//! Usage:
//!   resume-spike demo            – open the picker with a few fixed candidates
//!   resume-spike streamed        – stream candidates from a bounded channel
//!   resume-spike tabbed          – run_tabbed_picker with multiple agents and
//!                                  85 candidates in "All" (pi=70), to exercise
//!                                  full-tab search, Alt+P/Alt+N in-view scroll
//!                                  and Tab/Alt+Left/Alt+Right tab switching
//!   resume-spike tabbed-right     – same, with an explicit right-hand Preview
//!   resume-spike tabbed-cards     – same, with title + metadata Session cards
//!   resume-spike tree             – unified cross-agent tree with a relation row
//!   resume-spike tabbed-async     – run_tabbed_picker opens immediately on
//!                                  "pi"/"omp" while a simulated slow
//!                                  "codex" background agent adds its
//!                                  candidates ~900ms later, to exercise the
//!                                  BackgroundAgent pending header hint and
//!                                  live tab-list growth
//!   resume-spike preflight       – run preflight only, print result, exit
//!   resume-spike empty           – zero candidates
//!   resume-spike control-chars   – candidates carrying ANSI/OSC/bidi attacks
//!
//! The chosen opaque key is printed to stdout on selection as `key:<N>`.

use std::process::ExitCode;
use std::time::UNIX_EPOCH;

use resume::config::{PreviewMode, PreviewPosition};
use resume::picker::{
    self, CandidateKey, MIN_TERM_HEIGHT, MIN_TERM_WIDTH, PickerCandidate, PickerOutcome,
    run_picker, run_picker_streamed, run_tabbed_picker, run_tree_picker,
};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str).unwrap_or("demo") {
        "demo" => run(demo_candidates(), false),
        "streamed" => {
            let outcome = run_picker_streamed(demo_candidates(), false);
            print_outcome(outcome)
        }
        "tabbed" => print_outcome(run_tabbed_demo(PreviewPosition::Auto, false)),
        "tabbed-right" => print_outcome(run_tabbed_demo(PreviewPosition::Right, false)),
        "tabbed-cards" => print_outcome(run_tabbed_demo(PreviewPosition::Auto, true)),
        "tree" => print_outcome(run_tree_demo(false)),
        "relation-tabbed" => print_outcome(run_tree_demo(true)),
        "tabbed-async" => print_outcome(run_tabbed_async_demo()),
        "raw" => run(demo_candidates(), true),
        "empty" => run(Vec::new(), false),
        "control-chars" => run(control_attack_candidates(), false),
        "preflight" => match picker::preflight() {
            Ok(()) => {
                println!("preflight ok");
                ExitCode::SUCCESS
            }
            Err(reason) => {
                eprintln!("preflight failed: {reason}");
                ExitCode::from(2)
            }
        },
        "min-size" => {
            // Report the minimum supported size so tests can size the PTY.
            println!("{MIN_TERM_WIDTH}x{MIN_TERM_HEIGHT}");
            ExitCode::SUCCESS
        }
        other => {
            eprintln!("unknown subcommand: {other}");
            ExitCode::from(2)
        }
    }
}

fn run(candidates: Vec<(CandidateKey, String, String)>, force_raw: bool) -> ExitCode {
    print_outcome(run_picker(candidates, force_raw))
}

/// Builds a multi-agent fixture and drives `run_tabbed_picker` directly —
/// mirrors `app::run_interactive` once discovery has already fully completed.
/// "pi" gets 70 candidates, "claude" and "omp" a handful each, so "All" (85)
/// holds Sessions far older than one screen while Tab/Alt+Left/Alt+Right
/// cycle all 4 tabs.
fn run_tabbed_demo(position: PreviewPosition, cards: bool) -> PickerOutcome {
    let mut candidates = Vec::new();
    let mut next_id = 1u64;
    let mut push = |agent: &str, count: usize, candidates: &mut Vec<PickerCandidate>| {
        for i in 0..count {
            candidates.push(PickerCandidate {
                key: CandidateKey(next_id),
                display: if cards {
                    format!("{agent}-candidate-{i:03}\nSession metadata")
                } else {
                    format!("{agent}-candidate-{i:03}")
                },
                search_text: format!("{agent}-candidate-{i:03}"),
                preview: format!("Session {agent}-{i}"),
                details: Some(format!(
                    "USER INPUT\n\n1. Session input\n{}",
                    (1..=35)
                        .map(|n| format!("input line {n}\n"))
                        .collect::<String>()
                )),
                rank: (
                    0,
                    Some(UNIX_EPOCH + std::time::Duration::from_secs(next_id)),
                ),
                agent: agent.to_string(),
                selectable: true,
            });
            next_id += 1;
        }
    };
    push("pi", 70, &mut candidates);
    push("claude", 10, &mut candidates);
    push("omp", 5, &mut candidates);
    run_tabbed_picker(
        std::sync::Arc::new(std::sync::Mutex::new(candidates)),
        PreviewMode::Hidden,
        position,
        None,
    )
}
/// Cross-agent tree fixture with a non-selectable relation-only row.
/// The separate tabbed variant exercises normal-view acceptance guards.
fn run_tree_demo(tabs_enabled: bool) -> PickerOutcome {
    let row = |id: u64, agent: &str, name: &str, selectable: bool| PickerCandidate {
        key: CandidateKey(id),
        display: name.to_string(),
        search_text: name.to_string(),
        preview: format!("Session {name}"),
        details: Some(format!("USER INPUT\n\n1. Session input\n{name}\n")),
        rank: (0, Some(UNIX_EPOCH + std::time::Duration::from_secs(id))),
        agent: agent.to_string(),
        selectable,
    };
    let candidates = vec![
        row(1, "pi", "tree pi-root", true),
        row(2, "omp", "tree   omp-child", true),
        row(3, "pi", "tree pi-relation-only", false),
    ];
    let picker = if tabs_enabled {
        run_tabbed_picker
    } else {
        run_tree_picker
    };
    picker(
        std::sync::Arc::new(std::sync::Mutex::new(candidates)),
        PreviewMode::Hidden,
        PreviewPosition::Auto,
        None,
    )
}

/// Opens the picker on "pi"/"omp" immediately, while a simulated slow
/// "codex" background agent adds 5 candidates to the *same shared list*
/// ~900ms later -- mirrors `app::run_interactive`'s split when Codex is
/// configured alongside other agents. Exercises the `BackgroundAgent`
/// pending header hint and picking up a background agent's tab once it
/// has produced its first Session.
fn run_tabbed_async_demo() -> PickerOutcome {
    let mut candidates = Vec::new();
    let mut next_id = 1u64;
    let mut push = |agent: &str, count: usize, candidates: &mut Vec<PickerCandidate>| {
        for i in 0..count {
            candidates.push(PickerCandidate {
                key: CandidateKey(next_id),
                display: format!("{agent}-candidate-{i:03}"),
                search_text: format!("{agent}-candidate-{i:03}"),
                preview: format!("Session {agent}-{i}"),
                details: Some(format!(
                    "USER INPUT\n\n1. Session input\n{}",
                    (1..=35)
                        .map(|n| format!("input line {n}\n"))
                        .collect::<String>()
                )),
                rank: (
                    0,
                    Some(UNIX_EPOCH + std::time::Duration::from_secs(next_id)),
                ),
                agent: agent.to_string(),
                selectable: true,
            });
            next_id += 1;
        }
    };
    push("pi", 3, &mut candidates);
    push("omp", 3, &mut candidates);
    let candidates = std::sync::Arc::new(std::sync::Mutex::new(candidates));
    let pending = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    {
        let candidates = candidates.clone();
        let pending = pending.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(900));
            let mut codex_candidates = Vec::new();
            for (next_id, i) in (1000u64..).zip(0..5) {
                codex_candidates.push(PickerCandidate {
                    key: CandidateKey(next_id),
                    display: format!("codex-candidate-{i:03}"),
                    search_text: format!("codex-candidate-{i:03}"),
                    preview: format!("Session codex-{i}"),
                    details: Some(format!(
                        "USER INPUT\n\n1. Session input\n{}",
                        (1..=35)
                            .map(|n| format!("input line {n}\n"))
                            .collect::<String>()
                    )),
                    rank: (
                        0,
                        Some(UNIX_EPOCH + std::time::Duration::from_secs(next_id)),
                    ),
                    agent: "codex".to_string(),
                    selectable: true,
                });
            }
            candidates.lock().unwrap().extend(codex_candidates);
            pending.store(false, std::sync::atomic::Ordering::SeqCst);
        });
    }
    run_tabbed_picker(
        candidates,
        PreviewMode::Hidden,
        PreviewPosition::Auto,
        Some(picker::BackgroundAgent {
            label: "codex".to_string(),
            pending,
        }),
    )
}

fn print_outcome(outcome: PickerOutcome) -> ExitCode {
    match outcome {
        PickerOutcome::Selected(key) => {
            println!("key:{}", key.0);
            ExitCode::SUCCESS
        }
        PickerOutcome::Cancelled => {
            println!("cancelled");
            ExitCode::SUCCESS
        }
        PickerOutcome::Interrupted => {
            eprintln!("interrupted");
            ExitCode::from(130)
        }
        PickerOutcome::PreflightFailed(reason) => {
            eprintln!("preflight failed: {reason}");
            ExitCode::from(2)
        }
        PickerOutcome::InternalError(reason) => {
            eprintln!("internal error: {reason}");
            ExitCode::from(1)
        }
    }
}

fn demo_candidates() -> Vec<(CandidateKey, String, String)> {
    vec![
        (
            CandidateKey(1),
            "pi  fix login bug".into(),
            "Session 1\nworkspace: /tmp/proj\nfirst user message about a login bug".into(),
        ),
        (
            CandidateKey(2),
            "claude  refactor parser".into(),
            "Session 2\nworkspace: /tmp/other\nrefactoring the JSONL parser".into(),
        ),
        (
            CandidateKey(3),
            "codex  add tests".into(),
            "Session 3\nworkspace: /tmp/codex\nadding rollout tests".into(),
        ),
    ]
}

fn control_attack_candidates() -> Vec<(CandidateKey, String, String)> {
    // Each candidate carries a different terminal-control attack in both its
    // display text and its preview. The sanitizer must neutralize all of them
    // so none is ever executed.
    vec![
        (
            CandidateKey(10),
            "\x1b[31mred\x1b[0m ANSI color".into(),
            "\x1b]8;;https://evil.example\x1b\\click\x1b]8;;\x1b\\ OSC-8 hyperlink".into(),
        ),
        (
            CandidateKey(11),
            "title\x1b]0;PWNED\x07 set".into(),
            "\x1b[2J\x1b[H clear-screen + cursor-home".into(),
        ),
        (
            CandidateKey(12),
            "file\x1b[5Cgap cursor-forward".into(),
            "bidi: file\u{202e}txt.exe RLO override".into(),
        ),
        (
            CandidateKey(13),
            "clip\x1b]52;c;Zm9v\x07 OSC-52".into(),
            "c1: a\u{9b}31mb single-byte CSI".into(),
        ),
    ]
}
