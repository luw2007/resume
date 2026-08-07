//! Step 2 spike entry point: a tiny CLI exercised by the PTY tests.
//!
//! Usage:
//!   resume-spike demo            – open the picker with a few fixed candidates
//!   resume-spike streamed        – stream candidates from a bounded channel
//!   resume-spike preflight       – run preflight only, print result, exit
//!   resume-spike empty           – zero candidates
//!   resume-spike control-chars   – candidates carrying ANSI/OSC/bidi attacks
//!
//! The chosen opaque key is printed to stdout on selection as `key:<N>`.

use std::process::ExitCode;

use resume::picker::{
    self, CandidateKey, MIN_TERM_HEIGHT, MIN_TERM_WIDTH, PickerOutcome, run_picker,
    run_picker_streamed,
};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str).unwrap_or("demo") {
        "demo" => run(demo_candidates(), false),
        "streamed" => {
            let outcome = run_picker_streamed(demo_candidates(), false);
            print_outcome(outcome)
        }
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
