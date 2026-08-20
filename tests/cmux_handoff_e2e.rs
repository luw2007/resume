#![cfg(unix)]

use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use std::{
    fs,
    io::{Read, Write},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

fn executable(path: &Path, body: &str) {
    fs::write(path, body).unwrap();
    let mut p = fs::metadata(path).unwrap().permissions();
    p.set_mode(0o755);
    fs::set_permissions(path, p).unwrap();
}
fn binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_resume"))
}
fn wait_for(
    rx: &mpsc::Receiver<u8>,
    needle: &[u8],
    deadline: Duration,
    seen: &mut Vec<u8>,
) -> bool {
    let end = Instant::now() + deadline;
    while Instant::now() < end {
        if let Ok(b) = rx.recv_timeout(Duration::from_millis(50)) {
            seen.push(b);
            if seen.windows(needle.len()).any(|w| w == needle) {
                return true;
            }
        }
    }
    false
}
fn run_e2e(report_failure: bool) -> (u32, Vec<u8>, PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let a = root.path().join("A");
    let b = root.path().join("B");
    fs::create_dir_all(&a).unwrap();
    fs::create_dir_all(&b).unwrap();
    Command::new("git")
        .args(["init", "-q"])
        .current_dir(&a)
        .status()
        .unwrap();
    let home = root.path().join("home");
    fs::create_dir_all(home.join(".resume")).unwrap();
    fs::write(
        home.join(".resume/settings.json"),
        r#"{"schema_version":1,"agents":["pi"],"known_agents":["pi"]}"#,
    )
    .unwrap();
    Command::new("git")
        .args(["-C", a.to_str().unwrap(), "config", "user.email", "e@e"])
        .status()
        .unwrap();
    Command::new("git")
        .args(["-C", a.to_str().unwrap(), "config", "user.name", "e"])
        .status()
        .unwrap();
    Command::new("git")
        .args(["-C", a.to_str().unwrap(), "add", "."])
        .status()
        .unwrap();
    Command::new("git")
        .args(["-C", a.to_str().unwrap(), "commit", "-qm", "init"])
        .status()
        .unwrap();
    fs::create_dir_all(home.join(".pi/agent")).unwrap();
    let session_dir = home.join(".pi/agent/sessions");
    fs::create_dir_all(&session_dir).unwrap();
    let grouped = session_dir.clone();
    fs::create_dir_all(&grouped).unwrap();
    fs::write(grouped.join("session.jsonl"), format!("{{\"type\":\"session\",\"v\":3,\"id\":\"e2e\",\"cwd\":\"{}\",\"timestamp\":1700000000}}\n{{\"type\":\"session_info\",\"name\":\"e2e candidate\"}}\n",b.display())).unwrap();
    let bin = root.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    let state = root.path().join("state");
    fs::write(&state, a.canonicalize().unwrap().display().to_string()).unwrap();
    let log = root.path().join("cmux.log");
    let marker = root.path().join("pi.marker");
    let cmux = bin.join("cmux");
    executable(
        &cmux,
        &format!(
            r#"#!/bin/sh
printf '%s\n' "$*" >> '{}'
if [ "$1" = identify ]; then printf '{{"caller":{{"workspace_id":"W","surface_id":"S"}},"app_cli_path":"{}"}}\n'
elif [ "$1" = workspace ]; then printf '{{"workspaces":[{{"id":"W","current_directory":"%s"}}]}}' "$(cat '{}')"
elif [ "$1" = rpc ]; then if [ "{}" = 1 ]; then exit 1; fi; printf '%s' '{}' > '{}'; fi
"#,
            log.display(),
            cmux.display(),
            state.display(),
            if report_failure { "1" } else { "0" },
            b.canonicalize().unwrap().display(),
            state.display()
        ),
    );
    let pi = bin.join("pi");
    executable(
        &pi,
        &format!(
            "#!/bin/sh\nif [ \"$(cat '{}' )\" = '{}' ]; then printf '%s\\n' \"$PWD\" > '{}'; fi\nsleep 1\n",
            state.display(),
            b.canonicalize().unwrap().display(),
            marker.display()
        ),
    );
    let mut cmd = CommandBuilder::new(binary());
    cmd.cwd(&a);
    cmd.env_clear();
    cmd.env("HOME", &home);
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    cmd.env("PATH", path);
    cmd.env("TERM", "xterm-256color");
    cmd.env("RESUME_DISABLE_PROC_PROBE", "1");
    cmd.env("PI_CODING_AGENT_DIR", home.join(".pi/agent"));
    cmd.env("PI_CODING_AGENT_SESSION_DIR", &session_dir);
    cmd.env("PI_CODING_AGENT_SESSION_DIR", &session_dir);
    cmd.env("XDG_CONFIG_HOME", home.join("xdg/config"));
    cmd.env("XDG_DATA_HOME", home.join("xdg/data"));
    cmd.env("XDG_STATE_HOME", home.join("xdg/state"));
    cmd.env("XDG_CACHE_HOME", home.join("xdg/cache"));
    cmd.env("CMUX_WORKSPACE_ID", "W");
    cmd.env("CMUX_SURFACE_ID", "S");
    cmd.arg("-a");
    cmd.arg("pi");
    cmd.arg("--down");
    cmd.arg("all");
    cmd.arg("--no-confirm");
    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 30,
            cols: 120,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();
    let mut child = pair.slave.spawn_command(cmd).unwrap();
    let mut writer = pair.master.take_writer().unwrap();
    let mut reader = pair.master.try_clone_reader().unwrap();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut b = [0; 4096];
        while let Ok(n) = reader.read(&mut b) {
            if n == 0 {
                break;
            }
            for x in &b[..n] {
                let _ = tx.send(*x);
            }
        }
    });
    let mut seen = Vec::new();
    assert!(
        wait_for(&rx, b"e2e candidate", Duration::from_secs(10), &mut seen),
        "candidate not rendered: {:?}",
        String::from_utf8_lossy(&seen)
    );
    writer.write_all(b"\r").unwrap();
    let end = Instant::now() + Duration::from_secs(15);
    let status;
    loop {
        if let Some(s) = child.try_wait().unwrap() {
            status = s.exit_code();
            break;
        }
        if Instant::now() > end {
            let _ = child.kill();
            status = 127;
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
    thread::sleep(Duration::from_millis(50));
    (status, seen, log)
}
#[test]
fn cmux_resume_real_pty_handoff_success_and_report_failure() {
    if !std::env::var("SPIKE_PTY_TESTS")
        .map(|v| v == "1" || v == "true")
        .unwrap_or(false)
    {
        eprintln!("skipping: PTY E2E tests disabled");
        return;
    }
    let (status, _, log) = run_e2e(false);
    assert_eq!(status, 0);
    let calls = fs::read_to_string(log).unwrap();
    assert!(
        calls.contains("identify")
            && calls.contains("workspace list")
            && calls.contains("rpc surface.report_pwd")
    );
    let (status, _, _) = run_e2e(true);
    assert_ne!(status, 0);
}
