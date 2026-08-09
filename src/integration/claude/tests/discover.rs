#![allow(unused_imports)]
use crate::integration::claude::test_support::*;
use crate::{
    integration::claude,
    jsonl,
    session::{ActivityStatus, SupportStatus, WorkspaceEvidence},
    snapshot,
};
use std::{
    ffi::{OsStr, OsString},
    fs,
    path::{Path, PathBuf},
};
#[test]
fn workspace_key_collision_pair_keeps_distinct_cwd() {
    let home = tempfile::tempdir().unwrap();
    let root = claude::resolve_root(None, Some(home.path())).unwrap();

    let cwd_a = home.path().join("repo-a");
    let cwd_b = home.path().join("repo-b");
    fs::create_dir_all(&cwd_a).unwrap();
    fs::create_dir_all(&cwd_b).unwrap();

    // Two different encodings of a workspace path that could collide.
    write_transcript(
        &default_root_dir(home.path()),
        "-Users-example-repo",
        UUID_A,
        &standard_records(UUID_A, cwd_a.to_str().unwrap(), "message A"),
    );
    write_transcript(
        &default_root_dir(home.path()),
        "-Users-example-repo-", // trailing separator variant
        UUID_A,
        &standard_records(UUID_A, cwd_b.to_str().unwrap(), "message B"),
    );

    let snap_before = snapshot_tree(home.path());
    let discovery = claude::discover(&root).unwrap();
    let snap_after = snapshot_tree(home.path());

    // Both Sessions retained because their cwd evidence is distinct, even
    // though the workspace keys would collide if reversed.
    assert_eq!(
        discovery.sessions.len(),
        2,
        "both distinct-cwd sessions kept"
    );

    let cwds: Vec<PathBuf> = discovery
        .sessions
        .iter()
        .filter_map(|session| session.workspace.workspace().map(Path::to_path_buf))
        .collect();
    // The recorded cwd is stored verbatim from the transcript (not
    // canonicalized), so compare against the paths as written.
    assert!(cwds.contains(&cwd_a), "cwd_a retained: {cwds:?}");
    assert!(cwds.contains(&cwd_b), "cwd_b retained: {cwds:?}");

    // Read-only: no bytes or mtimes changed.
    snapshot::assert_unchanged(&snap_before, &snap_after);
}

// ===========================================================================
// Fixture: UUID agreement / disagreement
// ===========================================================================

#[cfg(unix)]
#[test]
fn symlinked_transcript_inside_effective_root_is_read() {
    let home = tempfile::tempdir().unwrap();
    let root = claude::resolve_root(None, Some(home.path())).unwrap();
    let cwd = home.path().join("work");
    fs::create_dir_all(&cwd).unwrap();
    let target = write_transcript(
        &default_root_dir(home.path()),
        "target-key",
        UUID_A,
        &standard_records(UUID_A, cwd.to_str().unwrap(), "followed safely"),
    );
    let link_dir = default_root_dir(home.path()).join("projects/link-key");
    fs::create_dir_all(&link_dir).unwrap();
    std::os::unix::fs::symlink(&target, link_dir.join(format!("{UUID_A}.jsonl"))).unwrap();
    fs::remove_file(target).unwrap();
    let relocated = default_root_dir(home.path()).join("relocated.data");
    write_transcript(
        &default_root_dir(home.path()),
        "unused",
        UUID_A,
        &standard_records(UUID_A, cwd.to_str().unwrap(), "followed safely"),
    );
    let generated = default_root_dir(home.path())
        .join("projects/unused")
        .join(format!("{UUID_A}.jsonl"));
    fs::rename(generated, &relocated).unwrap();
    fs::remove_dir_all(default_root_dir(home.path()).join("projects/unused")).unwrap();
    fs::remove_file(link_dir.join(format!("{UUID_A}.jsonl"))).unwrap();
    std::os::unix::fs::symlink(&relocated, link_dir.join(format!("{UUID_A}.jsonl"))).unwrap();

    let discovery = claude::discover(&root).unwrap();
    assert_eq!(discovery.sessions.len(), 1);
    assert_eq!(
        discovery.sessions[0].title.as_deref(),
        Some("followed safely")
    );
    assert!(discovery.diagnostics.is_empty());
}

#[cfg(unix)]
#[test]
fn symlinked_transcript_outside_effective_root_is_rejected_with_diagnostic() {
    let home = tempfile::tempdir().unwrap();
    let root = claude::resolve_root(None, Some(home.path())).unwrap();
    let cwd = home.path().join("work");
    fs::create_dir_all(&cwd).unwrap();
    let outside = tempfile::tempdir().unwrap();
    let target = write_transcript(
        outside.path(),
        "foreign",
        UUID_A,
        &standard_records(UUID_A, cwd.to_str().unwrap(), "must not leak"),
    );
    let link_dir = default_root_dir(home.path()).join("projects/key");
    fs::create_dir_all(&link_dir).unwrap();
    std::os::unix::fs::symlink(&target, link_dir.join(format!("{UUID_A}.jsonl"))).unwrap();

    let discovery = claude::discover(&root).unwrap();
    assert!(discovery.sessions.is_empty());
    assert!(discovery.diagnostics.iter().any(|d| {
        d.category == "claude_io"
            && d.verbose_chain
                .as_deref()
                .is_some_and(|chain| chain.contains("outside effective root"))
    }));
}

#[test]
fn uuid_filename_and_embedded_session_id_agree_is_supported() {
    let home = tempfile::tempdir().unwrap();
    let root = claude::resolve_root(None, Some(home.path())).unwrap();
    let cwd = home.path().join("work");
    fs::create_dir_all(&cwd).unwrap();

    write_transcript(
        &default_root_dir(home.path()),
        "key",
        UUID_A,
        &standard_records(UUID_A, cwd.to_str().unwrap(), "hello"),
    );

    let discovery = claude::discover(&root).unwrap();
    assert_eq!(discovery.sessions.len(), 1);
    let session = &discovery.sessions[0];
    assert_eq!(session.support, SupportStatus::Supported);
    assert_eq!(session.resumable_id, OsString::from(UUID_A));
}

#[test]
fn uuid_filename_and_embedded_session_id_disagree_with_cwd_is_discover_only() {
    let home = tempfile::tempdir().unwrap();
    let root = claude::resolve_root(None, Some(home.path())).unwrap();
    let cwd = home.path().join("work");
    fs::create_dir_all(&cwd).unwrap();

    // Filename is UUID_A, embedded is UUID_B — they disagree. A cwd exists.
    write_transcript(
        &default_root_dir(home.path()),
        "key",
        UUID_A,
        &standard_records(UUID_B, cwd.to_str().unwrap(), "hello"),
    );

    let discovery = claude::discover(&root).unwrap();
    assert_eq!(discovery.sessions.len(), 1, "retained as Discover Only");
    let session = &discovery.sessions[0];
    assert_eq!(
        session.support,
        SupportStatus::DiscoverOnly,
        "cannot safely resume under a mismatched ID"
    );
    // Resumable ID is the embedded one, never the filename (not authoritative).
    assert_eq!(session.resumable_id, OsString::from(UUID_B));
}

#[test]
fn uuid_disagreement_without_cwd_is_skipped() {
    let home = tempfile::tempdir().unwrap();
    let root = claude::resolve_root(None, Some(home.path())).unwrap();

    // Filename UUID_A, embedded UUID_B, no cwd at all.
    write_transcript(
        &default_root_dir(home.path()),
        "key",
        UUID_A,
        &[json(&[
            ("type", str_val("user")),
            ("sessionId", str_val(UUID_B)),
            (
                "message",
                json(&[("role", str_val("user")), ("content", str_val("hi"))]),
            ),
        ])],
    );

    let discovery = claude::discover(&root).unwrap();
    assert!(
        discovery.sessions.is_empty(),
        "no safe identity and no workspace: skipped entirely"
    );
    assert!(
        discovery
            .diagnostics
            .iter()
            .any(|d| d.category == "claude_identity_disagreement"),
        "a disagreement diagnostic is emitted"
    );
}

#[test]
fn no_embedded_session_id_with_cwd_is_discover_only() {
    let home = tempfile::tempdir().unwrap();
    let root = claude::resolve_root(None, Some(home.path())).unwrap();
    let cwd = home.path().join("work");
    fs::create_dir_all(&cwd).unwrap();

    // UUID-like filename but no embedded sessionId anywhere.
    write_transcript(
        &default_root_dir(home.path()),
        "key",
        UUID_A,
        &[json(&[
            ("type", str_val("user")),
            ("cwd", str_val(cwd.to_str().unwrap())),
            (
                "message",
                json(&[("role", str_val("user")), ("content", str_val("hi"))]),
            ),
        ])],
    );

    let discovery = claude::discover(&root).unwrap();
    assert_eq!(discovery.sessions.len(), 1);
    assert_eq!(
        discovery.sessions[0].support,
        SupportStatus::DiscoverOnly,
        "filename alone is not authoritative"
    );
}

#[test]
fn uuid_agreement_is_case_and_brace_insensitive() {
    let home = tempfile::tempdir().unwrap();
    let root = claude::resolve_root(None, Some(home.path())).unwrap();
    let cwd = home.path().join("work");
    fs::create_dir_all(&cwd).unwrap();

    // Filename uppercase, embedded with braces, both equal when normalized.
    let upper = UUID_A.to_uppercase();
    let braced = format!("{{{UUID_A}}}");
    write_transcript(
        &default_root_dir(home.path()),
        "key",
        &upper,
        &standard_records(&braced, cwd.to_str().unwrap(), "hi"),
    );

    let discovery = claude::discover(&root).unwrap();
    assert_eq!(discovery.sessions.len(), 1);
    assert_eq!(discovery.sessions[0].support, SupportStatus::Supported);
}

// ===========================================================================
// Fixture: title precedence variants
// ===========================================================================

#[test]
fn nested_subagent_artifacts_are_not_independent_sessions() {
    let home = tempfile::tempdir().unwrap();
    let root = claude::resolve_root(None, Some(home.path())).unwrap();
    let cwd = home.path().join("work");
    fs::create_dir_all(&cwd).unwrap();

    // A valid top-level transcript.
    write_transcript(
        &default_root_dir(home.path()),
        "key",
        UUID_A,
        &standard_records(UUID_A, cwd.to_str().unwrap(), "top level"),
    );

    // A nested subagent transcript under the same workspace-key. It must NOT
    // be surfaced as an independent Session.
    let subagents_dir = default_root_dir(home.path())
        .join("projects")
        .join("key")
        .join("subagents");
    fs::create_dir_all(&subagents_dir).unwrap();
    let sub_path = subagents_dir.join(format!("{UUID_B}.jsonl"));
    fs::write(
        &sub_path,
        serde_json::to_string(&json(&[
            ("type", str_val("user")),
            ("sessionId", str_val(UUID_B)),
            ("cwd", str_val(cwd.to_str().unwrap())),
            (
                "message",
                json(&[("role", str_val("user")), ("content", str_val("nested"))]),
            ),
        ]))
        .unwrap()
            + "\n",
    )
    .unwrap();

    let discovery = claude::discover(&root).unwrap();
    assert_eq!(
        discovery.sessions.len(),
        1,
        "nested subagent transcript is not an independent Session"
    );
    assert_eq!(discovery.sessions[0].resumable_id, OsString::from(UUID_A));
}

// ===========================================================================
// Fixture: read-only regression (no bytes/mtimes changed)
// ===========================================================================

#[test]
fn discovery_does_not_modify_any_file_or_directory() {
    let home = tempfile::tempdir().unwrap();
    let root = claude::resolve_root(None, Some(home.path())).unwrap();
    let cwd = home.path().join("work");
    fs::create_dir_all(&cwd).unwrap();
    write_transcript(
        &default_root_dir(home.path()),
        "key",
        UUID_A,
        &standard_records(UUID_A, cwd.to_str().unwrap(), "hello"),
    );

    let snap_before = snapshot_tree(home.path());
    let _discovery = claude::discover(&root).unwrap();
    let snap_after = snapshot_tree(home.path());

    snapshot::assert_unchanged(&snap_before, &snap_after);
}

// ===========================================================================
// Fixture: empty / non-existent projects dir
// ===========================================================================

#[test]
fn missing_projects_dir_yields_empty_discovery() {
    let home = tempfile::tempdir().unwrap();
    let root = claude::resolve_root(None, Some(home.path())).unwrap();
    let discovery = claude::discover(&root).unwrap();
    assert!(discovery.sessions.is_empty());
    assert!(discovery.diagnostics.is_empty());
}

#[test]
fn empty_projects_dir_yields_empty_discovery() {
    let home = tempfile::tempdir().unwrap();
    fs::create_dir_all(default_root_dir(home.path()).join("projects")).unwrap();
    let root = claude::resolve_root(None, Some(home.path())).unwrap();
    let discovery = claude::discover(&root).unwrap();
    assert!(discovery.sessions.is_empty());
}

#[test]
fn non_jsonl_files_are_ignored() {
    let home = tempfile::tempdir().unwrap();
    let key_dir = default_root_dir(home.path()).join("projects").join("key");
    fs::create_dir_all(&key_dir).unwrap();
    fs::write(key_dir.join("README.md"), "not a session").unwrap();
    fs::write(key_dir.join("notes.txt"), "also not").unwrap();

    let root = claude::resolve_root(None, Some(home.path())).unwrap();
    let discovery = claude::discover(&root).unwrap();
    assert!(discovery.sessions.is_empty());
}

// ===========================================================================
// Fixture: identity collision across workspace keys
// ===========================================================================

/// The same UUID under two different workspace-key directories represents two
/// independent Sessions only if their provenance (effective root + native
// locator path) differs. The native_locator is the transcript path.
#[test]
fn same_uuid_under_two_workspace_keys_keeps_both_as_distinct_native_locators() {
    let home = tempfile::tempdir().unwrap();
    let root = claude::resolve_root(None, Some(home.path())).unwrap();
    let cwd_a = home.path().join("a");
    let cwd_b = home.path().join("b");
    fs::create_dir_all(&cwd_a).unwrap();
    fs::create_dir_all(&cwd_b).unwrap();

    write_transcript(
        &default_root_dir(home.path()),
        "key-one",
        UUID_A,
        &standard_records(UUID_A, cwd_a.to_str().unwrap(), "in one"),
    );
    write_transcript(
        &default_root_dir(home.path()),
        "key-two",
        UUID_A,
        &standard_records(UUID_A, cwd_b.to_str().unwrap(), "in two"),
    );

    let discovery = claude::discover(&root).unwrap();
    assert_eq!(discovery.sessions.len(), 2, "distinct native locators");
    // Keys differ because native_locator (the transcript path) differs.
    assert_ne!(discovery.sessions[0].key, discovery.sessions[1].key);
}

// ===========================================================================
// Activity is Unknown
// ===========================================================================

#[test]
fn activity_is_unknown_absent_positive_correlation() {
    let home = tempfile::tempdir().unwrap();
    let root = claude::resolve_root(None, Some(home.path())).unwrap();
    let cwd = home.path().join("work");
    fs::create_dir_all(&cwd).unwrap();
    write_transcript(
        &default_root_dir(home.path()),
        "key",
        UUID_A,
        &standard_records(UUID_A, cwd.to_str().unwrap(), "hello"),
    );

    let discovery = claude::discover(&root).unwrap();
    assert_eq!(discovery.sessions.len(), 1);
    assert_eq!(discovery.sessions[0].activity, ActivityStatus::Unknown);
}

// ===========================================================================
// Root resolution edge cases
// ===========================================================================
