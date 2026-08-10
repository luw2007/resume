//! Synthetic fixture generators for benchmarks. Never reads or writes real
//! agent data; every fixture is a fresh `TempDir`.

use std::{
    io::Write,
    path::{Path, PathBuf},
    process::Command,
};

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

/// Build a synthetic Pi/OMP-shaped grouped session tree:
/// `<session_root>/<workspace-key>/session-*.jsonl`, each starting with a v3
/// header record followed by a configurable number of user-message records,
/// plus optional large outlier files. Mirrors `codex_tree`'s shape parameters
/// (file count, average lines, large-file count/size, Workspace fan-out) so
/// its benchmark results are directly comparable to the Codex group.
///
/// Header/message record shapes match
/// `src/integration/pi/test_support.rs::header_v3`/`user_message_string` and
/// `src/integration/omp/format.rs`'s v3 header (both integrations share the
/// same v3 header/message shape; OMP additionally tolerates a preceding
/// title-sidecar record, which this fixture does not need to exercise the
/// common discovery path).
pub fn grouped_session_tree(
    session_root: &Path,
    files: usize,
    avg_lines: usize,
    big_files: usize,
    big_file_mb: usize,
) -> PathBuf {
    std::fs::create_dir_all(session_root).expect("mkdir session_root");
    let unique_workspaces = (files as f64).sqrt().round().clamp(1.0, 20.0) as usize;

    for i in 0..files {
        let workspace_dir = session_root.join(format!("workspace-{}", i % unique_workspaces));
        std::fs::create_dir_all(&workspace_dir).unwrap();
        let path = workspace_dir.join(format!("session-{i:05}.jsonl"));
        let cwd = workspace_dir.clone();
        write_grouped_session(&path, &format!("pi-synth-{i:05}"), &cwd, avg_lines);
    }
    for i in 0..big_files {
        let workspace_dir = session_root.join("workspace-0");
        std::fs::create_dir_all(&workspace_dir).unwrap();
        let path = workspace_dir.join(format!("session-big-{i:03}.jsonl"));
        write_big_grouped_session(
            &path,
            &format!("pi-synth-big-{i:03}"),
            &workspace_dir,
            big_file_mb,
        );
    }
    session_root.to_path_buf()
}

fn write_grouped_session(path: &Path, id: &str, cwd: &Path, lines: usize) {
    let mut f = std::fs::File::create(path).expect("create session");
    writeln!(
        f,
        r#"{{"type":"session","v":3,"id":"{id}","cwd":"{}","timestamp":1700000000}}"#,
        cwd.display()
    )
    .unwrap();
    for n in 0..lines {
        writeln!(
            f,
            r#"{{"type":"user","timestamp":{},"message":{{"role":"user","content":"synthetic benchmark user input number {n}, not real Session content, padded to a representative length xxxxxxxxxxxxxxxxxxxx"}}}}"#,
            1_700_000_010 + n as u64
        )
        .unwrap();
    }
}

fn write_big_grouped_session(path: &Path, id: &str, cwd: &Path, target_mb: usize) {
    let mut f = std::fs::File::create(path).expect("create big session");
    writeln!(
        f,
        r#"{{"type":"session","v":3,"id":"{id}","cwd":"{}","timestamp":1700000000}}"#,
        cwd.display()
    )
    .unwrap();
    let line = r#"{"type":"assistant","timestamp":1700000020,"message":{"role":"assistant","content":"synthetic filler assistant output, not a real Session, repeated to reach the target file size for benchmarking large-transcript parse cost xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"}}
"#;
    let target_bytes = target_mb * 1024 * 1024;
    let mut written = 0usize;
    while written < target_bytes {
        f.write_all(line.as_bytes()).unwrap();
        written += line.len();
    }
}

/// Build a synthetic Claude-shaped project tree:
/// `<claude_root>/projects/<workspace-key>/<uuid>.jsonl`, one workspace-key
/// directory per Session (Claude's real on-disk shape encodes the Workspace
/// into the directory name; discovery does not reverse it, so this fixture
/// does not need a realistic encoding, only a distinct directory per
/// Session). Each transcript's filename UUID matches its embedded
/// `sessionId`, satisfying Claude's exact-identity contract
/// (`src/integration/claude/format.rs`: "filename UUID and embedded
/// sessionId must agree").
pub fn claude_project_tree(
    claude_root: &Path,
    files: usize,
    avg_lines: usize,
    big_files: usize,
    big_file_mb: usize,
) -> PathBuf {
    let projects = claude_root.join("projects");
    std::fs::create_dir_all(&projects).expect("mkdir projects");
    let unique_workspaces = (files as f64).sqrt().round().clamp(1.0, 20.0) as usize;

    for i in 0..files {
        let workspace_dir = projects.join(format!("-tmp-bench-ws-{}", i % unique_workspaces));
        std::fs::create_dir_all(&workspace_dir).unwrap();
        let uuid = synthetic_uuid(i);
        let path = workspace_dir.join(format!("{uuid}.jsonl"));
        let cwd = format!("/tmp/bench-ws-{}", i % unique_workspaces);
        write_claude_transcript(&path, &uuid, &cwd, avg_lines);
    }
    for i in 0..big_files {
        let workspace_dir = projects.join("-tmp-bench-ws-0");
        std::fs::create_dir_all(&workspace_dir).unwrap();
        let uuid = synthetic_uuid(files + i);
        let path = workspace_dir.join(format!("{uuid}.jsonl"));
        write_big_claude_transcript(&path, &uuid, "/tmp/bench-ws-0", big_file_mb);
    }
    claude_root.to_path_buf()
}

/// A deterministic UUID-v4-shaped string derived from an index, distinct per
/// call, matching Claude's filename/`sessionId` UUID-shape identity check.
fn synthetic_uuid(index: usize) -> String {
    format!("{index:08x}-cafe-4bee-8bad-f00dfacade{index:02x}")
}

fn write_claude_transcript(path: &Path, uuid: &str, cwd: &str, lines: usize) {
    let mut f = std::fs::File::create(path).expect("create transcript");
    // The first record carries sessionId + cwd, matching
    // src/integration/claude/test_support.rs::standard_records.
    writeln!(
        f,
        r#"{{"type":"user","sessionId":"{uuid}","cwd":"{cwd}","message":{{"role":"user","content":"Fix the login bug"}}}}"#
    )
    .unwrap();
    for n in 0..lines {
        writeln!(
            f,
            r#"{{"type":"user","message":{{"role":"user","content":"synthetic benchmark user input number {n}, not real Session content xxxxxxxxxxxxxxxxxxxx"}}}}"#
        )
        .unwrap();
    }
}

fn write_big_claude_transcript(path: &Path, uuid: &str, cwd: &str, target_mb: usize) {
    let mut f = std::fs::File::create(path).expect("create big transcript");
    writeln!(
        f,
        r#"{{"type":"user","sessionId":"{uuid}","cwd":"{cwd}","message":{{"role":"user","content":"Fix the login bug"}}}}"#
    )
    .unwrap();
    let line = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"synthetic filler assistant output, not a real Session, repeated to reach the target file size for benchmarking large-transcript parse cost xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"}]}}
"#;
    let target_bytes = target_mb * 1024 * 1024;
    let mut written = 0usize;
    while written < target_bytes {
        f.write_all(line.as_bytes()).unwrap();
        written += line.len();
    }
}
