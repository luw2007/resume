//! Step 2 spike entry point: a tiny CLI exercised by the PTY tests.
//!
//! Usage:
//!   resume-spike demo            – open the picker with a few fixed candidates
//!   resume-spike streamed        – stream candidates from a bounded channel
//!   resume-spike prod-slow       – production picker fed by a channel whose
//!                                  sender stays open past selection, to
//!                                  reproduce/guard against the picker
//!                                  blocking its return on slow discovery
//!   resume-spike preflight       – run preflight only, print result, exit
//!   resume-spike empty           – zero candidates
//!   resume-spike control-chars   – candidates carrying ANSI/OSC/bidi attacks
//!
//! The chosen opaque key is printed to stdout on selection as `key:<N>`.

use std::process::ExitCode;
use std::time::Duration;

use resume::config::{PreviewMode, PreviewPosition};
use resume::picker::{
    self, CandidateKey, MIN_TERM_HEIGHT, MIN_TERM_WIDTH, PickerCandidate, PickerOutcome,
    run_picker, run_picker_streamed, run_production_picker,
};

/// How long the `prod-slow` producer keeps its sender open after emitting
/// the one candidate. Must comfortably exceed the assertion bound the PTY
/// test uses, so a regression (picker blocking on this sender) is unmissable
/// even under CI scheduler contention.
const SLOW_DISCOVERY_HOLD: Duration = Duration::from_secs(5);

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str).unwrap_or("demo") {
        "demo" => run(demo_candidates(), false),
        "streamed" => {
            let outcome = run_picker_streamed(demo_candidates(), false);
            print_outcome(outcome)
        }
        "prod-slow" => print_outcome(run_prod_slow()),
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

/// Mirrors `app::run_interactive`'s use of `run_production_picker`, but the
/// producer thread deliberately holds its sender open for
/// `SLOW_DISCOVERY_HOLD` after emitting the single candidate — modeling a
/// discovery worker that is still scanning when the user selects. The picker
/// must return as soon as Skim reports a selection, independent of when this
/// sender eventually drops.
fn run_prod_slow() -> PickerOutcome {
    let (tx, rx) = std::sync::mpsc::sync_channel::<PickerCandidate>(1);
    std::thread::spawn(move || {
        let _ = tx.send(PickerCandidate {
            key: CandidateKey(1),
            display: "pi  slow-candidate".into(),
            search_text: "pi  slow-candidate".into(),
            preview: "Session 1\nstill discovering other agents".into(),
        });
        std::thread::sleep(SLOW_DISCOVERY_HOLD);
        // `tx` drops here, unblocking the picker's internal producer thread
        // if it is still waiting on `recv()`. The picker itself must not
        // have waited for this.
    });
    run_production_picker(rx, PreviewMode::Hidden, PreviewPosition::Auto)
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
