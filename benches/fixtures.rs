//! Synthetic fixture generators for benchmarks. Never reads or writes real
//! agent data; every fixture is a fresh `TempDir`.

use std::{io::Write, path::Path, process::Command};

use tempfile::TempDir;

/// Build a synthetic `CODEX_HOME`-shaped rollout tree:
/// `sessions/YYYY/MM/DD/rollout-<n>.jsonl`, each starting with a valid
/// `session_meta` record (so it is a discoverable Session) followed by
/// `avg_lines` `event_msg` user-message records, plus `big_files` additional
/// rollouts padded to `big_file_mb` MiB with repeated Codex-shaped filler
/// records -- emulating the small number of very large rollouts (tens of MB)
/// observed in real long-lived `~/.codex/sessions` directories, without
/// including any real content.
///
/// `cwd` fans in across only `sqrt(files)`-ish distinct workspaces (bounded
/// to at least 1, at most 20) rather than one-per-file, matching the
/// real-world shape where hundreds of rollouts share a handful of project
/// directories -- the shape that made per-Session `git rev-parse` so costly
/// before the Scope cache fix this suite guards.
pub fn codex_tree(files: usize, avg_lines: usize, big_files: usize, big_file_mb: usize) -> TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let sessions = dir.path().join("sessions/2026/01/01");
    std::fs::create_dir_all(&sessions).expect("mkdir");

    let unique_workspaces = (files as f64).sqrt().round().clamp(1.0, 20.0) as usize;

    for i in 0..files {
        let path = sessions.join(format!("rollout-synthetic-{i:05}.jsonl"));
        let cwd = dir
            .path()
            .join(format!("workspace-{}", i % unique_workspaces));
        write_rollout(&path, &format!("codex-synth-{i:05}"), &cwd, avg_lines);
    }
    for i in 0..big_files {
        let path = sessions.join(format!("rollout-synthetic-big-{i:03}.jsonl"));
        let cwd = dir.path().join("workspace-0");
        write_big_rollout(&path, &format!("codex-synth-big-{i:03}"), &cwd, big_file_mb);
    }
    dir
}

fn write_rollout(path: &Path, id: &str, cwd: &Path, lines: usize) {
    let mut f = std::fs::File::create(path).expect("create rollout");
    writeln!(
        f,
        r#"{{"type":"session_meta","payload":{{"id":"{id}","cwd":"{}","timestamp":"2026-01-01T00:00:00Z"}}}}"#,
        cwd.display()
    )
    .unwrap();
    for n in 0..lines {
        writeln!(
            f,
            r#"{{"type":"event_msg","payload":{{"type":"user_message","message":"synthetic benchmark user input number {n}, not real Session content, padded to a representative length for realistic parse cost xxxxxxxxxxxxxxxxxxxx"}}}}"#
        )
        .unwrap();
    }
}

fn write_big_rollout(path: &Path, id: &str, cwd: &Path, target_mb: usize) {
    let mut f = std::fs::File::create(path).expect("create big rollout");
    writeln!(
        f,
        r#"{{"type":"session_meta","payload":{{"id":"{id}","cwd":"{}","timestamp":"2026-01-01T00:00:00Z"}}}}"#,
        cwd.display()
    )
    .unwrap();
    let line = r#"{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"synthetic filler assistant output, not a real Session, repeated to reach the target file size for benchmarking large-rollout parse cost xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"}]}}
"#;
    let target_bytes = target_mb * 1024 * 1024;
    let mut written = 0usize;
    while written < target_bytes {
        f.write_all(line.as_bytes()).unwrap();
        written += line.len();
    }
}

/// A real (not mocked) temporary Git repository, for benchmarking the actual
/// `git rev-parse` subprocess path.
pub fn init_git_repo() -> TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let status = Command::new("git")
        .args(["init", "-q"])
        .current_dir(dir.path())
        .status()
        .expect("git init");
    assert!(status.success(), "git init failed");
    dir
}
