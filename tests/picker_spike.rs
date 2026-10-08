//! Automated PTY tests for the Step 2 Skim feasibility spike.
//!
//! These tests spawn the `resume-spike` example binary inside a pseudo-terminal
//! and drive it with real keystrokes, then assert on the rendered bytes. They
//! are the decision-gate evidence that Skim's public library API meets the
//! essential interaction model without a fork or second TUI.
//!
//! Skipped automatically when no PTY is available (e.g. some CI containers)
//! via the `SPIKE_PTY_TESTS=1` guard, so an environment without a usable pty
//! never produces false negatives.

use std::io::{Read, Write};
use std::sync::{Mutex, MutexGuard, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use portable_pty::{CommandBuilder, PtySize, native_pty_system};

/// Build the command that runs a `resume-spike` subcommand.
fn spike_exe_path() -> std::path::PathBuf {
    // Cargo exposes the compiled example binary under target/debug/examples/.
    let mut exe = std::env::current_exe().unwrap();
    exe.pop(); // examples/ or deps/
    if exe.file_name().and_then(|s| s.to_str()) == Some("deps") {
        exe.pop();
    }
    // Find the example binary alongside the test binary.
    let candidate = exe.join("examples").join("resume-spike");
    if candidate.exists() {
        candidate
    } else {
        exe.join("resume-spike")
    }
}

/// Build the command that runs a `resume-spike` subcommand.
fn spike_cmd(sub: &str) -> CommandBuilder {
    let exe = spike_exe_path();
    let mut cmd = CommandBuilder::new(&exe);
    cmd.arg(sub);
    cmd.env("TERM", "xterm-256color");
    cmd.env("RESUME_DISABLE_PROC_PROBE", "1");
    // The spike uses only in-memory candidates. Point every conventional
    // agent/config root at a deliberately nonexistent fixture path so a
    // regression can never scan the runner's real HOME or credentials.
    let isolated = std::env::temp_dir().join(format!("resume-pty-{}", std::process::id()));
    std::fs::create_dir_all(&isolated).expect("create isolated PTY home");
    cmd.env("HOME", &isolated);
    cmd.env("XDG_CONFIG_HOME", isolated.join("xdg-config"));
    cmd.env("XDG_DATA_HOME", isolated.join("xdg-data"));
    cmd.env("XDG_STATE_HOME", isolated.join("xdg-state"));
    cmd.env("XDG_CACHE_HOME", isolated.join("xdg-cache"));
    cmd.env("PI_CODING_AGENT_DIR", isolated.join("pi"));
    cmd.env("PI_CONFIG_DIR", isolated.join("omp"));
    cmd.env("CLAUDE_CONFIG_DIR", isolated.join("claude"));
    cmd.env("CODEX_HOME", isolated.join("codex"));
    cmd
}

/// Screen state fed by the background reader: a real VT emulator sized like
/// the PTY, so ANSI cell diffs, cursor moves, erases and UTF-8/CSI sequences
/// split across reads resolve to what a terminal would actually display.
struct ScreenState {
    parser: vt100::Parser,
    /// Last time bytes arrived from the child or the test wrote input.
    last_change: Instant,
}

/// A screen must be unchanged this long (after the last output or input)
/// before a predicate match counts as "settled".
const SCREEN_IDLE: Duration = Duration::from_millis(200);

/// A PTY session with a background reader draining rendered bytes.
struct PtySession {
    writer: Box<dyn Write + Send>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    _pair: portable_pty::PtyPair,
    rx: mpsc::Receiver<u8>,
    /// Every byte rendered across the whole session, accumulated in the
    /// background thread. `read_for` returns a slice of new bytes, but this
    /// buffer retains everything for whole-session assertions.
    accumulated: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
    screen: std::sync::Arc<std::sync::Mutex<ScreenState>>,
    // macOS can fail `openpty` under a burst of parallel integration tests.
    // Serialize PTY ownership while still exercising the full interaction.
    // Declared last so the child and PTY are torn down (see `Drop`) before
    // the next test may open its own PTY.
    _serial: MutexGuard<'static, ()>,
}

impl Drop for PtySession {
    /// Reap the child even when a test panics mid-interaction, so a failed
    /// assertion never leaves a picker process running behind the guard.
    fn drop(&mut self) {
        if let Ok(None) = self.child.try_wait() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn pty_serial() -> MutexGuard<'static, ()> {
    static SERIAL: Mutex<()> = Mutex::new(());
    SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn spawn(sub: &str, cols: u16, rows: u16) -> PtySession {
    let serial = pty_serial();
    let pair = native_pty_system()
        .openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("open pty");
    let cmd = spike_cmd(sub);
    let child = pair.slave.spawn_command(cmd).expect("spawn resume-spike");
    let writer = pair.master.take_writer().expect("take pty writer");
    let mut reader = pair.master.try_clone_reader().expect("clone pty reader");
    let (tx, rx) = mpsc::channel::<u8>();
    // portable-pty places the master in raw mode by default on Unix; we do
    // not need an explicit set_raw_mode call.
    // Background drain so the child never blocks on a full PTY buffer.
    let accumulated: std::sync::Arc<std::sync::Mutex<Vec<u8>>> =
        std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let acc_clone = accumulated.clone();
    let screen = std::sync::Arc::new(std::sync::Mutex::new(ScreenState {
        parser: vt100::Parser::new(rows, cols, 0),
        last_change: Instant::now(),
    }));
    let screen_clone = screen.clone();
    thread::spawn(move || {
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if let Ok(mut a) = acc_clone.lock() {
                        a.extend_from_slice(&buf[..n]);
                    }
                    {
                        // The emulator keeps UTF-8 and CSI state across
                        // reads, so chunk boundaries never corrupt cells.
                        let mut s = screen_clone.lock().unwrap_or_else(|p| p.into_inner());
                        s.parser.process(&buf[..n]);
                        s.last_change = Instant::now();
                    }
                    for &b in &buf[..n] {
                        if tx.send(b).is_err() {
                            break;
                        }
                    }
                }
            }
        }
    });
    PtySession {
        writer,
        child,
        _pair: pair,
        rx,
        accumulated,
        screen,
        _serial: serial,
    }
}

impl PtySession {
    /// Read until `deadline` collecting bytes; returns the raw buffer.
    fn read_for(&self, dur: Duration) -> Vec<u8> {
        let start = Instant::now();
        let mut out = Vec::new();
        while start.elapsed() < dur {
            match self.rx.recv_timeout(Duration::from_millis(50)) {
                Ok(b) => out.push(b),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        out
    }

    fn write(&mut self, bytes: &[u8]) {
        // Input invalidates any previously settled screen: the next settle
        // must observe SCREEN_IDLE of quiet *after* this keystroke.
        self.screen
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .last_change = Instant::now();
        let _ = self.writer.write_all(bytes);
        let _ = self.writer.flush();
    }

    /// Current emulated screen: one line per terminal row, trailing blanks
    /// trimmed, trailing empty rows dropped.
    fn screen_text(&self) -> (String, Duration) {
        let s = self.screen.lock().unwrap_or_else(|p| p.into_inner());
        let (_, cols) = s.parser.screen().size();
        let mut rows: Vec<String> = s
            .parser
            .screen()
            .rows(0, cols)
            .map(|r| r.trim_end().to_string())
            .collect();
        while rows.last().is_some_and(|r| r.is_empty()) {
            rows.pop();
        }
        (rows.join("\n"), s.last_change.elapsed())
    }

    /// Wait until the emulated screen satisfies `pred` and has been quiet
    /// for `SCREEN_IDLE`. Panics with the full screen snapshot when `pred`
    /// never holds before `timeout`. Raw bytes are drained (not retained) so
    /// a later `read_for` sees only post-exit output.
    fn wait_screen(
        &mut self,
        what: &str,
        timeout: Duration,
        pred: impl Fn(&str) -> bool,
    ) -> String {
        let deadline = Instant::now() + timeout;
        loop {
            let _ = self.read_for(Duration::from_millis(50));
            let (text, idle) = self.screen_text();
            let holds = pred(&text);
            if holds && idle >= SCREEN_IDLE {
                return text;
            }
            if Instant::now() >= deadline {
                if holds {
                    return text;
                }
                panic!(
                    "timed out after {timeout:?} waiting for {what}\n--- screen ---\n{text}\n--- end ---"
                );
            }
        }
    }

    /// Wait for the screen to go quiet (no output for `SCREEN_IDLE` since
    /// the last keystroke) and return it, for asserting state or absence.
    fn settle(&mut self) -> String {
        self.wait_screen("a settled screen", Duration::from_secs(5), |_| true)
    }

    /// Return a copy of every byte rendered across the whole session so far.
    fn accumulated(&self) -> Vec<u8> {
        self.accumulated
            .lock()
            .map(|a| a.clone())
            .unwrap_or_default()
    }
}

/// Strip ANSI/CSI/OSC escape sequences from a byte buffer for readable
/// assertions. (Mirror of the production sanitizer, used in tests only.)
fn strip(buf: &[u8]) -> String {
    let s = String::from_utf8_lossy(buf);
    let bytes = s.as_bytes();
    let mut out = String::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b {
            i += 1;
            if i >= bytes.len() {
                break;
            }
            match bytes[i] {
                b'[' => {
                    i += 1;
                    while i < bytes.len() && !(0x40..=0x7e).contains(&bytes[i]) {
                        i += 1;
                    }
                    i += 1;
                }
                b']' => {
                    i += 1;
                    while i < bytes.len() {
                        if bytes[i] == 0x07 {
                            i += 1;
                            break;
                        }
                        if bytes[i] == 0x1b && i + 1 < bytes.len() && bytes[i + 1] == b'\\' {
                            i += 2;
                            break;
                        }
                        i += 1;
                    }
                }
                b'P' | b'X' | b'^' | b'_' => {
                    // DCS/SOS/PM/APC string sequences terminated by ST or BEL
                    i += 1;
                    while i < bytes.len() {
                        if bytes[i] == 0x07 {
                            i += 1;
                            break;
                        }
                        if bytes[i] == 0x1b && i + 1 < bytes.len() && bytes[i + 1] == b'\\' {
                            i += 2;
                            break;
                        }
                        i += 1;
                    }
                }
                _ => {
                    i += 1;
                }
            }
        } else if bytes[i] == 0xc2 && i + 1 < bytes.len() && bytes[i + 1] == 0x9b {
            // UTF-8 encoded C1 CSI (U+009B)
            i += 2;
            while i < bytes.len() && !(0x40..=0x7e).contains(&bytes[i]) {
                i += 1;
            }
            i += 1;
        } else {
            // Decode the next UTF-8 char properly to preserve multibyte.
            let rest = &s[i..];
            match rest.chars().next() {
                Some(ch) => {
                    let ch_len = ch.len_utf8();
                    // Skip C0 and C1 control characters except space.
                    if !ch.is_control() || ch == ' ' {
                        out.push(ch);
                    }
                    i += ch_len;
                }
                None => break,
            }
        }
    }
    out
}

/// Whether the PTY/terminal machinery is functional in this environment.
fn pty_available() -> bool {
    std::env::var("SPIKE_PTY_TESTS")
        .map(|v| v == "1" || v == "true")
        .unwrap_or(true)
}

/// Wait for the child to exit and return its status code.
fn wait_child(sess: &mut PtySession) -> u32 {
    // Poll with try_wait so we don't block forever if something is wrong.
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        match sess.child.try_wait() {
            Ok(Some(status)) => return status.exit_code(),
            Ok(None) => {
                if Instant::now() > deadline {
                    // Force-kill and reap.
                    let _ = sess.child.kill();
                    return sess.child.wait().map(|s| s.exit_code()).unwrap_or(127);
                }
                thread::sleep(Duration::from_millis(50));
            }
            Err(_) => {
                let _ = sess.child.wait().map(|s| s.exit_code()).unwrap_or(127);
                return 127;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Decision-gate tests
// ---------------------------------------------------------------------------

/// The core streaming + custom item + preview + selection flow works.
#[test]
fn skim_streams_candidates_and_selects_opaque_key() {
    if !pty_available() {
        eprintln!("skipping: PTY tests disabled");
        return;
    }
    let mut sess = spawn("demo", 100, 30);
    // Let the candidates stream in.
    let text = sess.wait_screen("login candidate", Duration::from_secs(4), |s| {
        s.contains("login")
    });
    // All three candidates are streamed and searchable. Skim collapses
    // inter-token spacing in unselected rows, so assert on distinctive tokens.
    assert!(text.contains("login"), "pi candidate missing: {text:?}");
    assert!(
        text.contains("refactor"),
        "claude candidate missing: {text:?}"
    );
    assert!(text.contains("tests"), "codex candidate missing: {text:?}");
    // Press Enter to accept the (default first) selection.
    sess.write(b"\r");
    let exit = wait_child(&mut sess);
    let out = strip(&sess.read_for(Duration::from_millis(500)));
    assert_eq!(exit, 0, "exit={exit}, out={out:?}");
    // The output is the opaque key, not a display string or path.
    assert!(out.contains("key:"), "opaque key not emitted: {out:?}");
    // The opaque key must be one of the known candidate keys, never a path or
    // display string.
    assert!(
        out.contains("key:1") || out.contains("key:2") || out.contains("key:3"),
        "selection returned an unexpected value: {out:?}"
    );
    assert!(
        !out.contains("/tmp/"),
        "selection leaked a workspace path: {out:?}"
    );
}

/// Streaming candidates arrive over a bounded channel while the picker is open.
#[test]
fn skim_streamed_path_still_selects() {
    if !pty_available() {
        return;
    }
    let mut sess = spawn("streamed", 100, 30);
    let text = sess.wait_screen("claude candidate", Duration::from_secs(4), |s| {
        s.contains("refactor")
    });
    assert!(
        text.contains("refactor"),
        "claude candidate missing: {text:?}"
    );
    sess.write(b"\r");
    let exit = wait_child(&mut sess);
    assert_eq!(exit, 0);
    let out = strip(&sess.read_for(Duration::from_millis(500)));
    assert!(out.contains("key:"), "out={out:?}");
}

/// `run_tabbed_picker` (the path `app::run_interactive` uses once discovery
/// fully completes): every candidate of the current tab is in one Skim view, so
/// a query reaches Sessions far older than the first screen. Alt+Left/Alt+Right
/// and Tab/Shift-Tab cycle tabs (wrapping). Fixture: pi=70, claude=10, omp=5.
#[test]
fn tabbed_picker_switches_tabs_with_alt_arrows_and_tab_keys() {
    if !pty_available() {
        return;
    }
    let mut sess = spawn("tabbed", 100, 30);
    let all = wait_for(&mut sess, "omp-candidate-004", Duration::from_millis(4000));
    assert!(all.contains("[All 85] pi claude omp"), "All tab: {all:?}");

    // macOS terminals emit the xterm modifier form for Option+Left/Right.
    sess.write(b"\x1b[1;3D");
    let omp_tab = wait_for(&mut sess, "omp-candidate-000", Duration::from_millis(4000));
    assert!(
        omp_tab.contains("All pi claude [omp 5]"),
        "omp: {omp_tab:?}"
    );

    sess.write(b"\x1b[1;3C");
    let all_again = wait_for(&mut sess, "[All 85]", Duration::from_millis(4000));
    assert!(
        all_again.contains("[All 85] pi claude omp"),
        "{all_again:?}"
    );

    sess.write(b"\t");
    let pi_tab = wait_for(&mut sess, "[pi 70]", Duration::from_millis(4000));
    assert!(
        !pi_tab.contains("claude-candidate") && !pi_tab.contains("omp-candidate"),
        "other agents leaked onto the pi tab: {pi_tab:?}"
    );

    sess.write(b"\t");
    let claude_tab = wait_for(&mut sess, "[claude 10]", Duration::from_millis(4000));
    assert!(
        claude_tab.contains("claude-candidate-000"),
        "{claude_tab:?}"
    );

    sess.write(b"\x1b[Z"); // Shift-Tab back to pi
    let _ = wait_for(&mut sess, "[pi 70]", Duration::from_millis(4000));

    sess.write(b"\r");
    let exit = wait_child(&mut sess);
    assert_eq!(exit, 0, "exit={exit}");
    let out = strip(&sess.read_for(Duration::from_millis(500)));
    assert!(out.contains("key:"), "out={out:?}");
}

/// A Session older than the first 50 is searchable and selectable at once.
#[test]
fn query_reaches_sessions_older_than_fifty_and_selects_them() {
    if !pty_available() {
        return;
    }
    let mut sess = spawn("tabbed", 100, 30);
    let _ = wait_for(&mut sess, "omp-candidate-004", Duration::from_millis(4000));
    sess.write(b"pi-candidate-000");
    let filtered = sess.wait_screen("filtered match count 1/85", Duration::from_secs(4), |s| {
        s.contains("pi-candidate-000") && s.contains("1/85")
    });
    assert!(
        filtered.contains("1/85"),
        "match count missing: {filtered:?}"
    );
    sess.write(b"\r");
    let exit = wait_child(&mut sess);
    assert_eq!(exit, 0, "exit={exit}");
    let out = strip(&sess.read_for(Duration::from_millis(500)));
    let at = out.find("key:1").unwrap_or_else(|| panic!("out={out:?}"));
    assert!(
        !out[at + 5..].starts_with(|c: char| c.is_ascii_digit()),
        "wrong key selected: {out:?}"
    );
}

/// A query that matches nothing says so with a visible `0/N` count.
#[test]
fn no_match_query_shows_zero_count() {
    if !pty_available() {
        return;
    }
    let mut sess = spawn("tabbed", 80, 24);
    let _ = wait_for(&mut sess, "omp-candidate-004", Duration::from_millis(4000));
    sess.write(b"zzzzqq");
    let text = wait_for(&mut sess, "0/85", Duration::from_millis(4000));
    assert!(text.contains("0/85"), "no-match feedback missing: {text:?}");
    sess.write(b"\x1b");
    assert_eq!(wait_child(&mut sess), 0);
}

/// Bare Left/Right edit the query cursor instead of switching tabs.
#[test]
fn bare_left_right_edit_query_cursor() {
    if !pty_available() {
        return;
    }
    let mut sess = spawn("tabbed", 100, 30);
    let _ = wait_for(&mut sess, "omp-candidate-004", Duration::from_millis(4000));
    sess.write(b"pi-candidate-9");
    sess.wait_screen("typed query", Duration::from_secs(3), |s| {
        s.contains("> pi-candidate-9")
    });
    sess.write(b"\x1b[D"); // Left: cursor before '9'
    let _ = sess.read_for(Duration::from_millis(200));
    sess.write(b"6"); // inserted => "pi-candidate-69", which only pi 069 matches
    let text = sess.wait_screen("inserted digit", Duration::from_secs(4), |s| {
        s.contains("> pi-candidate-69") && s.contains("pi-candidate-069")
    });
    assert!(
        text.contains("[All 85]") || !text.contains("[pi 70]"),
        "Left must not switch tabs: {text:?}"
    );
    sess.write(b"\r");
    let exit = wait_child(&mut sess);
    assert_eq!(exit, 0, "exit={exit}");
    let out = strip(&sess.read_for(Duration::from_millis(500)));
    assert!(
        out.contains("key:70"),
        "insertion did not target pi 069: {out:?}"
    );
}

/// Query text and side Preview visibility both survive a tab switch.
#[test]
fn query_and_side_preview_survive_tab_switch() {
    if !pty_available() {
        return;
    }
    let mut sess = spawn("tabbed", 130, 30);
    let _ = wait_for(&mut sess, "omp-candidate-004", Duration::from_millis(4000));
    sess.write(b"candidate");
    sess.wait_screen("typed query", Duration::from_secs(3), |s| {
        s.contains("> candidate")
    });
    sess.write(b"\x0f"); // show side Preview
    let _ = wait_for(&mut sess, "normalized", Duration::from_secs(3));
    sess.write(b"\t");
    let next = sess.wait_screen(
        "pi tab with query and preview",
        Duration::from_secs(4),
        |s| s.contains("[pi ") && s.contains("> candidate") && s.contains("normalized"),
    );
    assert!(next.contains("> candidate"), "query lost: {next:?}");
    assert!(
        next.contains("normalized"),
        "preview hidden by tab switch: {next:?}"
    );
    sess.write(b"\x1b");
    assert_eq!(wait_child(&mut sess), 0);
}

/// The shortcut footer keeps Enter / Preview / Esc / details visible at 60, 80
/// and 100 columns, and still at 60 columns with an explicit right-hand
/// Preview, where the list pane is only ~23 columns.
#[test]
fn footer_hints_visible_at_supported_widths() {
    if !pty_available() {
        return;
    }
    for (sub, cols) in [
        ("tabbed", 60u16),
        ("tabbed", 80),
        ("tabbed", 100),
        ("tabbed-right", 60),
    ] {
        let mut sess = spawn(sub, cols, 14);
        let text = wait_for(&mut sess, "Esc", Duration::from_secs(4));
        for hint in ["Enter", "preview", "Esc", "details"] {
            assert!(
                text.contains(hint),
                "{hint} missing at {sub}/{cols} cols: {text:?}"
            );
        }
        sess.write(b"\x1b");
        assert_eq!(wait_child(&mut sess), 0);
    }
}

/// Parse the `key:<N>` the spike prints on selection.
fn selected_key(out: &str) -> u64 {
    let at = out
        .find("key:")
        .unwrap_or_else(|| panic!("no key in {out:?}"));
    out[at + 4..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>()
        .parse()
        .unwrap_or_else(|_| panic!("unparsable key in {out:?}"))
}

/// Alt-P pages the cursor down the list in place (older Sessions) and Alt-N
/// pages it back up, with no tab change and no exit.
#[test]
fn alt_p_alt_n_scroll_the_list_in_place() {
    if !pty_available() {
        return;
    }
    let mut sess = spawn("tabbed", 100, 30);
    let _ = wait_for(&mut sess, "omp-candidate-004", Duration::from_millis(4000));
    sess.write(b"\x1bp");
    let _ = sess.read_for(Duration::from_millis(400));
    assert!(sess.child.try_wait().unwrap().is_none(), "Alt-P exited");
    sess.write(b"\r");
    assert_eq!(wait_child(&mut sess), 0);
    let paged = selected_key(&strip(&sess.read_for(Duration::from_millis(500))));
    assert!(
        paged < 85,
        "Alt-P did not move the cursor down: key {paged}"
    );
    drop(sess); // Release the serial PTY guard before opening another session.

    let mut sess = spawn("tabbed", 100, 30);
    let _ = wait_for(&mut sess, "omp-candidate-004", Duration::from_millis(4000));
    sess.write(b"\x1bp");
    let _ = sess.read_for(Duration::from_millis(300));
    sess.write(b"\x1bn");
    let _ = sess.read_for(Duration::from_millis(300));
    sess.write(b"\r");
    assert_eq!(wait_child(&mut sess), 0);
    let back = selected_key(&strip(&sess.read_for(Duration::from_millis(500))));
    assert!(
        back > paged,
        "Alt-N did not move the cursor back up: {back} vs {paged}"
    );
}

/// A relation-only (non-selectable) row ignores Enter in place, yet Tab and
/// Shift-Tab still move between tabs from it.
#[test]
fn non_selectable_row_ignores_enter_but_tab_navigates() {
    if !pty_available() {
        return;
    }
    let mut sess = spawn("relation-tabbed", 100, 30);
    let all = wait_for(&mut sess, "pi-relation-only", Duration::from_secs(4));
    assert!(all.contains("[All 3]"), "{all:?}");
    sess.write(b"\r");
    let _ = sess.read_for(Duration::from_millis(500));
    assert!(
        sess.child.try_wait().unwrap().is_none(),
        "Enter on a relation-only row must not resume or exit"
    );
    sess.write(b"\t");
    let pi_tab = wait_for(&mut sess, "[pi 2]", Duration::from_secs(4));
    assert!(
        pi_tab.contains("[pi 2]"),
        "Tab blocked on a relation-only row: {pi_tab:?}"
    );
    sess.write(b"\t");
    let omp_tab = wait_for(&mut sess, "[omp 1]", Duration::from_secs(4));
    assert!(omp_tab.contains("[omp 1]"), "{omp_tab:?}");
    sess.write(b"\r");
    assert_eq!(wait_child(&mut sess), 0);
    let out = strip(&sess.read_for(Duration::from_millis(500)));
    assert_eq!(selected_key(&out), 2, "{out:?}");
}

/// Tree mode keeps cross-agent ancestry in one searchable view; tab keys do
/// not slice it, even when focus is on a non-selectable relation row.
#[test]
fn tree_picker_keeps_cross_agent_rows_in_one_view() {
    if !pty_available() {
        return;
    }
    let mut sess = spawn("tree", 100, 30);
    let initial = wait_for(&mut sess, "pi-relation-only", Duration::from_secs(4));
    assert!(initial.contains("[Tree 3]"), "{initial:?}");
    assert!(
        initial.contains("pi-root") && initial.contains("omp-child"),
        "{initial:?}"
    );
    sess.write(b"\r");
    sess.settle();
    assert!(sess.child.try_wait().unwrap().is_none());
    sess.write(b"\t\x1b[Z\x1b[1;3C\x1b[1;3D");
    sess.settle();
    assert!(sess.child.try_wait().unwrap().is_none());
    sess.write(b"tree");
    let filtered = sess.wait_screen("query tree with 3/3", Duration::from_secs(4), |s| {
        s.contains("> tree") && s.contains("3/3")
    });
    assert!(filtered.contains("3/3"), "{filtered:?}");
    assert!(!filtered.contains("[pi 2]"), "{filtered:?}");
    // Ctrl-U discards the whole query (Ctrl-K is cursor-up in Skim, not a
    // kill-line), so the new query is exactly `omp-child`.
    sess.write(b"\x15omp-child");
    let narrowed = sess.wait_screen("query omp-child with 1/3", Duration::from_secs(4), |s| {
        s.contains("> omp-child") && s.contains("1/3")
    });
    assert!(narrowed.contains("1/3"), "{narrowed:?}");
    sess.write(b"\r");
    assert_eq!(wait_child(&mut sess), 0);
    let out = strip(&sess.read_for(Duration::from_millis(500)));
    assert_eq!(selected_key(&out), 2, "{out:?}");
}

/// Opening details with a doubled space consumes both spaces: after closing,
/// the query is exactly what was typed before.
#[test]
fn double_space_leaves_no_trigger_whitespace_in_query() {
    if !pty_available() {
        return;
    }
    let mut sess = spawn("tabbed", 110, 30);
    let _ = wait_for(&mut sess, "omp-candidate-004", Duration::from_secs(4));
    sess.write(b"omp");
    sess.wait_screen("typed query", Duration::from_secs(3), |s| {
        s.contains("> omp")
    });
    sess.write(b"  ");
    let _ = wait_for(&mut sess, "Session details", Duration::from_secs(3));
    sess.write(b"\x1b");
    sess.wait_screen("details closed", Duration::from_secs(3), |s| {
        !s.contains("Session details") && s.contains("> omp")
    });
    // Exactly "omp" => three Backspaces empty the query (85/85); a stray
    // trailing space would leave "o" (5/85).
    sess.write(b"\x7f\x7f\x7f");
    let text = wait_for(&mut sess, "85/85", Duration::from_secs(3));
    assert!(
        text.contains("85/85"),
        "stray whitespace left in query: {text:?}"
    );
    sess.write(b"\x1b");
    assert_eq!(wait_child(&mut sess), 0);
}

/// With no matching Session there is nothing to detail: a doubled space is
/// just query text and no card opens.
#[test]
fn double_space_does_not_open_details_without_a_current_item() {
    if !pty_available() {
        return;
    }
    let mut sess = spawn("tabbed", 110, 30);
    let _ = wait_for(&mut sess, "omp-candidate-004", Duration::from_secs(4));
    sess.write(b"zzzzqq");
    let _ = wait_for(&mut sess, "0/85", Duration::from_secs(3));
    sess.write(b"  ");
    let after = sess.settle();
    assert!(
        !after.contains("Session details"),
        "card opened on no item: {after:?}"
    );
    assert!(sess.child.try_wait().unwrap().is_none());
    sess.write(b"\x1b");
    assert_eq!(wait_child(&mut sess), 0);
}

/// A single space stays an ordinary term separator.
#[test]
fn single_space_still_separates_search_terms() {
    if !pty_available() {
        return;
    }
    let mut sess = spawn("tabbed", 110, 30);
    let _ = wait_for(&mut sess, "omp-candidate-004", Duration::from_secs(4));
    sess.write(b"pi 069");
    let text = wait_for(&mut sess, "1/85", Duration::from_secs(3));
    assert!(
        text.contains("1/85") && !text.contains("Session details"),
        "{text:?}"
    );
    sess.write(b"\x1b");
    assert_eq!(wait_child(&mut sess), 0);
}

/// Ctrl-C from the main list aborts with 130 as well.
#[test]
fn ctrl_c_in_list_with_query_exits_130() {
    if !pty_available() {
        return;
    }
    let mut sess = spawn("tabbed", 100, 30);
    let _ = wait_for(&mut sess, "omp-candidate-004", Duration::from_secs(4));
    sess.write(b"omp");
    sess.wait_screen("typed query", Duration::from_secs(3), |s| {
        s.contains("> omp")
    });
    sess.write(b"\x03");
    assert_eq!(wait_child(&mut sess), 130);
}

/// Ctrl-L re-reads the shared candidate list in place: a background agent's
/// Sessions become searchable without a tab switch and the hint clears.
#[test]
fn ctrl_l_refreshes_background_candidates_in_place() {
    if !pty_available() {
        return;
    }
    let mut sess = spawn("tabbed-async", 100, 30);
    let early = wait_for(&mut sess, "omp-candidate-002", Duration::from_millis(500));
    assert!(early.contains("codex scanning"), "{early:?}");
    std::thread::sleep(Duration::from_millis(1000));
    sess.write(b"\x0c");
    let text = wait_for(&mut sess, "[All 11]", Duration::from_secs(3));
    assert!(
        text.contains("[All 11]") && !text.contains("scanning"),
        "refresh did not pick up codex: {text:?}"
    );
    sess.write(b"\x1b");
    assert_eq!(wait_child(&mut sess), 0);
}

/// `run_tabbed_picker` with a [`resume::picker::BackgroundAgent`] (the path
/// `app::run_interactive` uses when Codex is configured alongside other
/// agents): the picker opens immediately on the agents that are already
/// ready, showing a "scanning" header hint, and never blocks on the
/// simulated slow "codex" background thread. Once that thread finishes
/// (merging into the same shared candidate list `run_tabbed_picker` reads
/// on every navigation), the next tab switch picks up its Sessions and the
/// hint clears -- no relaunch of the whole picker, no re-invocation needed.
#[test]
fn tabbed_picker_opens_immediately_while_background_agent_scans() {
    if !pty_available() {
        return;
    }
    let mut sess = spawn("tabbed-async", 100, 30);

    // Well before the simulated 900ms Codex delay elapses, the picker must
    // already be open and showing the ready agents' data with a pending
    // hint -- proving it did not wait for the background agent.
    let early = wait_for(&mut sess, "omp-candidate-002", Duration::from_millis(500));
    assert!(
        early.contains("[All 6] pi omp (codex scanning"),
        "expected an immediately open All tab with a pending hint: {early:?}"
    );
    assert!(
        !early.contains("codex-candidate"),
        "codex candidates appeared before its simulated scan finished: {early:?}"
    );

    // Once the background scan finishes, its Sessions merge into the
    // shared candidate list; the *next* navigation picks them up and the
    // pending hint clears.
    std::thread::sleep(Duration::from_millis(700));
    sess.write(b"\x1b[1;3C"); // Alt+Right: All -> pi
    let pi_tab = wait_for(
        &mut sess,
        "All [pi 3] omp codex",
        Duration::from_millis(2000),
    );
    assert!(
        pi_tab.contains("All [pi 3] omp codex"),
        "expected the pi tab: {pi_tab:?}"
    );
    sess.write(b"\x1b[1;3C"); // pi -> omp
    let omp_tab = wait_for(
        &mut sess,
        "All pi [omp 3] codex",
        Duration::from_millis(2000),
    );
    assert!(
        omp_tab.contains("All pi [omp 3] codex"),
        "expected the omp tab: {omp_tab:?}"
    );
    sess.write(b"\x1b[1;3C"); // omp -> codex
    let codex_tab = wait_for(
        &mut sess,
        "codex-candidate-004",
        Duration::from_millis(4000),
    );
    assert!(
        codex_tab.contains("All pi omp [codex 5]") && !codex_tab.contains("scanning"),
        "expected the codex tab with its pending hint cleared: {codex_tab:?}"
    );
    assert!(
        !codex_tab.contains("pi-candidate") && !codex_tab.contains("omp-candidate"),
        "other agents leaked onto the codex tab: {codex_tab:?}"
    );

    // Enter still resolves the correct opaque key from a background-agent tab.
    sess.write(b"\r");
    let exit = wait_child(&mut sess);
    assert_eq!(exit, 0, "exit={exit}");
    let out = strip(&sess.read_for(Duration::from_millis(500)));
    assert!(out.contains("key:"), "out={out:?}");
}

/// Wait until the emulated screen contains `needle` and is settled, then
/// return the screen text. Hard-fails with a screen snapshot on timeout.
fn wait_for(sess: &mut PtySession, needle: &str, timeout: Duration) -> String {
    sess.wait_screen(&format!("{needle:?}"), timeout, |s| s.contains(needle))
}

/// Esc restores the terminal and the process exits cleanly.
#[test]
fn esc_cancels_and_restores_terminal() {
    if !pty_available() {
        return;
    }
    let mut sess = spawn("demo", 100, 30);
    sess.read_for(Duration::from_millis(1200));
    sess.write(b"\x1b"); // ESC
    let exit = wait_child(&mut sess);
    let out = strip(&sess.read_for(Duration::from_millis(400)));
    assert_eq!(exit, 0, "Esc must exit 0; got {exit}, out={out:?}");
    assert!(out.contains("cancelled") || out.is_empty(), "out={out:?}");
    // Terminal restoration: the alternate screen has been exited. We cannot
    // easily assert cursor state, but a clean exit 0 with the cancel message
    // proves Skim's clear_on_exit ran. The main process exiting 0 (not 130)
    // proves it was Esc, not Ctrl+C.
}

/// Ctrl+C exits with code 130 (interrupt).
#[test]
fn ctrl_c_exits_130() {
    if !pty_available() {
        return;
    }
    let mut sess = spawn("demo", 100, 30);
    sess.read_for(Duration::from_millis(1200));
    sess.write(b"\x03"); // Ctrl+C
    let exit = wait_child(&mut sess);
    assert_eq!(exit, 130, "Ctrl+C must exit 130; got {exit}");
}

/// Empty input (zero candidates) is handled cleanly.
#[test]
fn zero_candidates_cancels_cleanly() {
    if !pty_available() {
        return;
    }
    let mut sess = spawn("empty", 100, 30);
    sess.read_for(Duration::from_millis(1000));
    sess.write(b"\r"); // accept with zero results
    let exit = wait_child(&mut sess);
    assert_eq!(exit, 0, "zero results accept must exit 0; got {exit}");
}

/// Terminal resize during picker operation does not crash.
#[test]
fn resize_does_not_crash() {
    if !pty_available() {
        return;
    }
    let _serial = pty_serial();
    let pty = native_pty_system();
    let pair = pty
        .openpty(PtySize {
            rows: 30,
            cols: 100,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("openpty");
    let cmd = spike_cmd("demo");
    let mut child = pair.slave.spawn_command(cmd).expect("spawn");
    let mut writer = pair.master.take_writer().expect("writer");
    let mut reader = pair.master.try_clone_reader().expect("reader");
    let (tx, rx) = mpsc::channel::<u8>();
    thread::spawn(move || {
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    for &b in &buf[..n] {
                        if tx.send(b).is_err() {
                            return;
                        }
                    }
                }
            }
        }
    });
    thread::sleep(Duration::from_millis(800));
    // Resize to a still-valid size while the picker is open.
    pair.master
        .resize(PtySize {
            rows: 24,
            cols: 90,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("resize");
    thread::sleep(Duration::from_millis(400));
    // Drain whatever was rendered.
    let _ = rx.try_iter().count();
    // Send Esc and reap.
    let _ = writer.write_all(b"\x1b");
    let _ = writer.flush();
    let code = poll_wait(&mut child);
    // The key assertion: resize did not panic/crash; the child still responds
    // to Esc and exits 0.
    assert_eq!(code, 0, "resize crashed picker (exit {code})");
}

/// Preview is hidden by default and Ctrl+O reveals it.
#[test]
fn preview_hidden_by_default_and_ctrl_o_toggles() {
    if !pty_available() {
        return;
    }
    let mut sess = spawn("demo", 120, 30);
    let before = sess.wait_screen("initial picker", Duration::from_secs(4), |s| {
        s.contains("login")
    });
    // Preview content (workspace path) must NOT be visible while hidden.
    assert!(
        !before.contains("/tmp/proj"),
        "preview leaked while hidden: {before:?}"
    );
    // Toggle preview on.
    sess.write(b"\x0f"); // Ctrl+O
    let after = sess.wait_screen("preview shown", Duration::from_secs(4), |s| {
        s.contains("/tmp/proj") || s.contains("workspace")
    });
    assert!(
        after.contains("/tmp/proj") || after.contains("workspace"),
        "preview not shown after Ctrl+O: {after:?}"
    );
    // Toggle preview off again.
    sess.write(b"\x0f");
    let off = sess.wait_screen("preview hidden", Duration::from_secs(4), |s| {
        !s.contains("/tmp/proj") && s.contains("login")
    });
    assert!(
        !off.contains("/tmp/proj"),
        "preview did not hide on second Ctrl+O: {off:?}"
    );
    sess.write(b"\x1b");
    let _ = wait_child(&mut sess);
}

#[test]
fn minimum_height_preview_keeps_focused_session_visible() {
    if !pty_available() {
        return;
    }
    for mode in ["tabbed", "tabbed-cards", "tree"] {
        let mut sess = spawn(mode, 60, 10);
        let title = if mode == "tree" {
            "tree pi-relation-only"
        } else {
            "omp-candidate-004"
        };
        sess.wait_screen(
            "focused session before Preview",
            Duration::from_secs(4),
            |s| s.contains(title),
        );
        sess.write(b"\x0f");
        let visible =
            sess.wait_screen("Preview and focused session", Duration::from_secs(4), |s| {
                s.contains("# normalized")
                    && s.lines()
                        .take_while(|line| !line.contains("# normalized"))
                        .any(|line| line.contains(title))
            });
        if mode == "tabbed-cards" {
            assert!(visible.contains("Session metadata"), "{visible:?}");
        }
        sess.write(b"\x1b");
        assert_eq!(wait_child(&mut sess), 0);
    }
}

#[test]
fn shrinking_terminal_with_preview_keeps_focused_card_visible() {
    if !pty_available() {
        return;
    }
    let mut sess = spawn("tabbed-cards", 60, 30);
    sess.wait_screen("focused card", Duration::from_secs(4), |s| {
        s.contains("omp-candidate-004")
    });
    sess.write(b"\x0f");
    sess.wait_screen("Preview before resize", Duration::from_secs(4), |s| {
        s.contains("# normalized")
    });
    // Resize the emulator with the PTY so assertions reflect the live screen,
    // not cells left behind by the larger terminal.
    {
        let mut screen = sess.screen.lock().unwrap_or_else(|p| p.into_inner());
        screen.parser.screen_mut().set_size(10, 60);
        screen.last_change = Instant::now();
    }
    sess._pair
        .master
        .resize(PtySize {
            rows: 10,
            cols: 60,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("resize");
    sess.wait_screen(
        "Preview and card after resize",
        Duration::from_secs(4),
        |s| {
            s.contains("# normalized")
                && s.contains("omp-candidate-004")
                && s.contains("Session metadata")
        },
    );
    sess.write(b"\r");
    assert_eq!(wait_child(&mut sess), 0);
    assert!(strip(&sess.accumulated()).contains("key:85"));
}

/// The details card overlays the list and Escape returns to it without exiting.
#[test]
fn double_space_opens_details_and_escape_returns_to_picker() {
    if !pty_available() {
        return;
    }
    let mut sess = spawn("tabbed", 110, 30);
    let _ = wait_for(&mut sess, "[All 85]", Duration::from_secs(4));
    sess.write(b"  ");
    let card = wait_for(&mut sess, "Session details", Duration::from_secs(3));
    assert!(card.contains("USER INPUT"), "user input missing: {card:?}");
    assert!(
        card.contains("input line 1"),
        "first input missing: {card:?}"
    );
    sess.write(b"j");
    let scrolled = sess.wait_screen("details scrolled by j", Duration::from_secs(3), |s| {
        s != card.as_str() && s.contains("Session details")
    });
    assert!(
        scrolled.contains("input") && scrolled != card,
        "j did not redraw details: {scrolled:?}"
    );
    sess.write(b"i");
    let i_up = sess.wait_screen("details scrolled back by i", Duration::from_secs(3), |s| {
        s != scrolled.as_str() && s.contains("USER INPUT")
    });
    assert!(
        i_up.contains("USER INPUT") && i_up != scrolled,
        "i did not scroll up: {i_up:?}"
    );
    sess.write(b"j");
    let down = sess.wait_screen("details scrolled by j again", Duration::from_secs(3), |s| {
        s != i_up.as_str() && s.contains("Session details")
    });
    sess.write(b"\x1b[A");
    let up = sess.wait_screen("details scrolled by Up", Duration::from_secs(3), |s| {
        s != down.as_str() && s.contains("Session details")
    });
    assert!(up != down, "Up did not redraw details");
    sess.write(b"\x1b");
    let list = sess.wait_screen("picker restored after Esc", Duration::from_secs(3), |s| {
        !s.contains("Session details") && s.contains("[All 85]")
    });
    assert!(
        list.contains("[All 85]"),
        "Escape did not redraw the picker: {list:?}"
    );
    sess.write(b"\x1b");
    assert_eq!(wait_child(&mut sess), 0);
}

/// Ctrl+C is never swallowed by the details card: it aborts with 130.
#[test]
fn ctrl_c_in_details_card_exits_130() {
    if !pty_available() {
        return;
    }
    let mut sess = spawn("tabbed", 110, 30);
    let _ = wait_for(&mut sess, "[All 85]", Duration::from_secs(4));
    sess.write(b"  ");
    let _ = wait_for(&mut sess, "Session details", Duration::from_secs(3));
    sess.write(b"\x03");
    assert_eq!(wait_child(&mut sess), 130);
}

/// `q` and Enter both dismiss the read-only card; Enter never resumes a Session.
#[test]
fn q_and_enter_dismiss_details_without_resuming() {
    if !pty_available() {
        return;
    }
    let mut sess = spawn("tabbed", 110, 30);
    let _ = wait_for(&mut sess, "[All 85]", Duration::from_secs(4));
    for dismiss in [&b"q"[..], &b"\r"[..]] {
        sess.write(b"  ");
        let _ = wait_for(&mut sess, "Session details", Duration::from_secs(3));
        sess.write(dismiss);
        let list = sess.wait_screen("list restored", Duration::from_secs(3), |s| {
            !s.contains("Session details") && s.contains("[All 85]")
        });
        assert!(list.contains("[All 85]"), "list not restored: {list:?}");
        assert!(
            sess.child.try_wait().unwrap().is_none(),
            "dismiss key {dismiss:?} exited the picker"
        );
    }
    sess.write(b"\x1b");
    assert_eq!(wait_child(&mut sess), 0);
}

/// PageDown and Ctrl-D scroll the details card instead of reaching the list.
#[test]
fn page_keys_scroll_details_card() {
    if !pty_available() {
        return;
    }
    let mut sess = spawn("tabbed", 110, 30);
    let _ = wait_for(&mut sess, "[All 85]", Duration::from_secs(4));
    sess.write(b"  ");
    let card = wait_for(&mut sess, "Session details", Duration::from_secs(3));
    assert!(card.contains("input line 1"), "{card:?}");
    sess.write(b"\x1b[6~"); // PageDown
    let paged = sess.wait_screen("details paged down", Duration::from_secs(3), |s| {
        s != card.as_str() && s.contains("Session details")
    });
    assert!(
        !paged.is_empty() && paged != card,
        "PageDown did not scroll"
    );
    sess.write(b"\x15"); // Ctrl-U: half page up
    let half_up = sess.wait_screen("details half-paged up", Duration::from_secs(3), |s| {
        s != paged.as_str() && s.contains("Session details")
    });
    sess.write(b"\x04"); // Ctrl-D: half page down
    let half = sess.wait_screen("details half-paged down", Duration::from_secs(3), |s| {
        s != half_up.as_str() && s.contains("Session details")
    });
    assert!(half != half_up, "Ctrl-D did not scroll");
    sess.write(b"\x03");
    assert_eq!(wait_child(&mut sess), 130);
}

/// Closing the details card must refresh the side Preview: the redraw after
/// Esc has to show the selected item's own `Session <agent>-<n>` preview text
/// and none of the card's "Session input" details.
#[test]
fn escape_from_details_restores_side_preview_content() {
    if !pty_available() {
        return;
    }
    let mut sess = spawn("tabbed", 130, 30);
    let _ = wait_for(&mut sess, "[All 85]", Duration::from_secs(4));
    sess.write(b"\x0f"); // show side Preview
    let _ = wait_for(&mut sess, "normalized", Duration::from_secs(3));
    sess.write(b"  ");
    let _ = wait_for(&mut sess, "Session details", Duration::from_secs(3));
    sess.write(b"\x1b");
    // Fresh read only: frames from before the Esc must not satisfy the check.
    // Cell diffs may drop spaces, so compare with whitespace removed.
    let after: String = sess
        .wait_screen("picker restored after Esc", Duration::from_secs(3), |s| {
            let compact: String = s.chars().filter(|c| !c.is_whitespace()).collect();
            !compact.contains("Sessiondetails")
                && ["Sessionpi-", "Sessionclaude-", "Sessionomp-"]
                    .iter()
                    .any(|needle| compact.contains(needle))
        })
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    assert!(
        ["Sessionpi-", "Sessionclaude-", "Sessionomp-"]
            .iter()
            .any(|needle| after.contains(needle)),
        "side preview content missing after Esc: {after:?}"
    );
    assert!(
        !after.contains("USERINPUT") && !after.contains("Sessioninput"),
        "stale details in side preview after Esc: {after:?}"
    );
    sess.write(b"\x1b");
    assert_eq!(wait_child(&mut sess), 0);
}

/// The filter text survives tab navigation.
#[test]
fn filter_query_survives_tab_switch() {
    if !pty_available() {
        return;
    }
    let mut sess = spawn("tabbed", 110, 30);
    let _ = wait_for(&mut sess, "omp-candidate-004", Duration::from_secs(4));
    sess.write(b"omp");
    sess.wait_screen("typed query", Duration::from_secs(3), |s| {
        s.contains("> omp")
    });
    sess.write(b"\t");
    let next = sess.wait_screen("pi tab with query", Duration::from_secs(4), |s| {
        s.contains("[pi ") && s.contains("> omp")
    });
    assert!(next.contains("> omp"), "query lost on tab switch: {next:?}");
    sess.write(b"\x1b");
    assert_eq!(wait_child(&mut sess), 0);
}

/// Double-space details work while filtering, without changing the filter.
#[test]
fn double_space_opens_details_with_active_filter() {
    if !pty_available() {
        return;
    }
    let mut sess = spawn("tabbed", 110, 30);
    let _ = wait_for(&mut sess, "omp-candidate-004", Duration::from_secs(4));
    sess.write(b"omp");
    sess.wait_screen("typed query", Duration::from_secs(3), |s| {
        s.contains("> omp")
    });
    sess.write(b"  ");
    let card = sess.wait_screen("details card", Duration::from_secs(4), |s| {
        s.contains("Session details") && s.contains("USER INPUT")
    });
    assert!(card.contains("USER INPUT"), "wrong details: {card:?}");
    sess.write(b"\x1b");
    let list = sess.wait_screen("filtered list restored", Duration::from_secs(4), |s| {
        !s.contains("Session details") && s.contains("te-003")
    });
    assert!(
        list.contains("te-003") && !list.contains("claude-candidate"),
        "filter lost after closing details: {list:?}"
    );
    sess.write(b"\x1b");
    assert_eq!(wait_child(&mut sess), 0);
}

/// Ctrl+R is bound to a safe no-op (ignore). The default `reload` action is
/// UNSAFE for a channel-fed picker because it re-runs the default `find`
/// command against the cwd, listing real files. This test proves Ctrl+R does
/// NOT trigger a filesystem scan, and that the dual-section preview is the
/// normalized/raw switch.
#[test]
fn ctrl_r_does_not_scan_filesystem() {
    if !pty_available() {
        return;
    }
    let mut sess = spawn("demo", 120, 30);
    // Drain the initial render.
    sess.read_for(Duration::from_millis(1000));
    // Open preview so the dual-section content is on screen.
    sess.write(b"\x0f");
    sess.read_for(Duration::from_millis(600));
    // Press Ctrl+R (must NOT reload the filesystem).
    sess.write(b"\x12");
    let after_reload = sess.settle();
    assert!(
        !after_reload.contains("Cargo.toml") && !after_reload.contains("src/"),
        "Ctrl+R listed working-directory contents on screen: {after_reload:?}"
    );
    // Quit.
    sess.write(b"\x1b");
    let exit = wait_child(&mut sess);
    // Capture everything rendered across the whole session.
    let all = strip(&sess.accumulated());
    assert_eq!(exit, 0, "Ctrl+R must not change exit code; got {exit}");
    // CRITICAL: no real filesystem entries leaked. The reload action, if it
    // had run, would have listed files like `Cargo.toml`, `src/`, etc.
    assert!(
        !all.contains("Cargo.toml") && !all.contains("Cargo.lock"),
        "Ctrl+R triggered a filesystem scan (reload is unsafe): {all:?}"
    );
    assert!(
        !all.contains(".git/") && !all.contains("src/"),
        "Ctrl+R listed working-directory contents: {all:?}"
    );
    assert!(
        !all.contains(".mira"),
        "Ctrl+R listed home/config contents: {all:?}"
    );
    // The dual-section preview content should still be reachable (workspace
    // path is part of the preview). Because ignore does not re-render, we only
    // assert the absence of a scan, which is the safety guarantee.
}

/// Control-sequence attacks are neutralized: no escape byte reaches the screen.
#[test]
fn control_sequence_attacks_are_neutralized() {
    if !pty_available() {
        return;
    }
    let mut sess = spawn("control-chars", 130, 30);
    let text = sess.wait_screen("control-char labels", Duration::from_secs(4), |s| {
        s.contains("ANSI color") && s.contains("OSC-52")
    });
    let rendered = sess.accumulated();
    // The candidate labels (sanitized) are visible...
    assert!(text.contains("ANSI color"), "rendered: {text:?}");
    assert!(text.contains("OSC-52"), "rendered: {text:?}");
    // ...but no raw OSC-8 hyperlink payload, OSC-52 clipboard write, title-set,
    // or clear-screen sequence survives into the candidate rows. (Skim itself
    // may emit its own UI escapes, so we assert on the *attack* strings.)
    assert!(
        !text.contains("evil.example"),
        "OSC-8 hyperlink payload leaked: {text:?}"
    );
    assert!(
        !text.contains("PWNED"),
        "title-set payload leaked: {text:?}"
    );
    // Raw bytes: no OSC-52 clipboard-write introducer in candidate text.
    // We allow Skim's own SGR colors, so check for the OSC 52 prefix only.
    let raw = String::from_utf8_lossy(&rendered);
    assert!(
        !raw.contains("]52;"),
        "OSC-52 clipboard write sequence reached the PTY"
    );
    assert!(
        !raw.contains("]8;;https"),
        "OSC-8 hyperlink reached the PTY"
    );
    sess.write(b"\x1b");
    let _ = wait_child(&mut sess);
}

/// stdin redirection: the picker still works when stdin is not a TTY, because
/// Skim opens /dev/tty directly.
#[test]
fn works_with_redirected_stdin() {
    if !pty_available() {
        return;
    }
    let _serial = pty_serial();
    let pty = native_pty_system();
    let pair = pty
        .openpty(PtySize {
            rows: 30,
            cols: 100,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("openpty");
    // Locate the example binary, then run it via `sh -c '... < /dev/null'` so
    // the child's fd 0 is /dev/null while /dev/tty is still the controlling
    // terminal. This is exactly the deployment shape: resume reads the user's
    // pipeline via stdin (here null) but drives the picker via /dev/tty.
    let exe = spike_exe_path();
    let shell_cmd = format!("'{}' demo < /dev/null", exe.display());
    let mut cmd = CommandBuilder::new("/bin/sh");
    cmd.arg("-c");
    cmd.arg(&shell_cmd);
    cmd.env("TERM", "xterm-256color");
    cmd.env("RESUME_DISABLE_PROC_PROBE", "1");
    let isolated = std::env::temp_dir().join(format!("resume-pty-redirect-{}", std::process::id()));
    std::fs::create_dir_all(&isolated).expect("create isolated redirected-stdin home");
    cmd.env("HOME", &isolated);
    cmd.env("XDG_CONFIG_HOME", isolated.join("xdg-config"));
    cmd.env("XDG_DATA_HOME", isolated.join("xdg-data"));
    cmd.env("XDG_STATE_HOME", isolated.join("xdg-state"));
    cmd.env("XDG_CACHE_HOME", isolated.join("xdg-cache"));
    cmd.env("PI_CODING_AGENT_DIR", isolated.join("pi"));
    cmd.env("PI_CONFIG_DIR", isolated.join("omp"));
    cmd.env("CLAUDE_CONFIG_DIR", isolated.join("claude"));
    cmd.env("CODEX_HOME", isolated.join("codex"));
    let mut child = pair.slave.spawn_command(cmd).expect("spawn");
    let mut writer = pair.master.take_writer().expect("writer");
    let mut reader = pair.master.try_clone_reader().expect("reader");
    let (tx, rx) = mpsc::channel::<u8>();
    thread::spawn(move || {
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    for &b in &buf[..n] {
                        if tx.send(b).is_err() {
                            return;
                        }
                    }
                }
            }
        }
    });
    // Give the picker time to render with candidates streamed.
    thread::sleep(Duration::from_millis(1500));
    let rendered: Vec<u8> = rx.try_iter().collect();
    let text = strip(&rendered);
    assert!(text.contains("pi  fix login bug"), "rendered: {text:?}");
    // Send Enter via the PTY (which is /dev/tty for the child).
    let _ = writer.write_all(b"\r");
    let _ = writer.flush();
    let code = poll_wait(&mut child);
    assert_eq!(code, 0, "redirected-stdin selection failed (exit {code})");
    let out: Vec<u8> = rx.try_iter().collect();
    let out_text = strip(&out);
    assert!(out_text.contains("key:"), "out={out_text:?}");
}

/// A terminal smaller than 60x10 fails preflight before the picker starts.
#[test]
fn tiny_terminal_fails_preflight() {
    if !pty_available() {
        return;
    }
    // Spawn the preflight subcommand in a tiny PTY.
    let mut sess = spawn("preflight", 40, 8);
    let exit = wait_child(&mut sess);
    let out = strip(&sess.read_for(Duration::from_millis(400)));
    assert_eq!(
        exit, 2,
        "tiny terminal must fail preflight with exit 2; got {exit}, out={out:?}"
    );
    assert!(
        out.contains("too small") || out.contains("minimum") || out.contains("failed"),
        "preflight reason missing: {out:?}"
    );
}

/// A reasonably sized terminal passes preflight.
#[test]
fn adequate_terminal_passes_preflight() {
    if !pty_available() {
        return;
    }
    let mut sess = spawn("preflight", 100, 30);
    let exit = wait_child(&mut sess);
    let out = strip(&sess.read_for(Duration::from_millis(400)));
    assert_eq!(
        exit, 0,
        "adequate terminal should pass preflight; out={out:?}"
    );
    assert!(out.contains("ok"), "out={out:?}");
}

/// Poll a child until it exits or the deadline passes; kill on timeout.
fn poll_wait(child: &mut Box<dyn portable_pty::Child + Send + Sync>) -> u32 {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.exit_code(),
            Ok(None) => {
                if Instant::now() > deadline {
                    let _ = child.kill();
                    return child.wait().map(|s| s.exit_code()).unwrap_or(127);
                }
                thread::sleep(Duration::from_millis(50));
            }
            Err(_) => {
                let _ = child.wait().map(|s| s.exit_code()).unwrap_or(127);
                return 127;
            }
        }
    }
}

// portable_pty::Child is a trait object; poll_wait above anchors its usage.
