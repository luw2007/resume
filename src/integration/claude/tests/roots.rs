#![allow(unused_imports)]
use crate::integration::claude::test_support::*;
use crate::{
    integration::claude,
    preview::jsonl,
    preview::snapshot,
    session::{ActivityStatus, SupportStatus, WorkspaceEvidence},
};
use std::{
    ffi::{OsStr, OsString},
    fs,
    path::{Path, PathBuf},
};
#[test]
fn alternate_root_is_resolved_and_marked_nondefault() {
    let custom = tempfile::tempdir().unwrap();
    let root = claude::resolve_root(Some(custom.path().as_os_str()), None).unwrap();
    assert!(root.nondefault);
    // The effective root is stored as given by the environment (resolve_root
    // does not canonicalize; canonicalization happens at read time).
    assert_eq!(root.effective_root, custom.path());

    let cwd = custom.path().join("work");
    fs::create_dir_all(&cwd).unwrap();
    write_transcript(
        custom.path(),
        "key",
        UUID_A,
        &standard_records(UUID_A, cwd.to_str().unwrap(), "hi"),
    );

    let discovery = claude::discover(&root).unwrap();
    assert_eq!(discovery.sessions.len(), 1);
    // The effective root is part of identity.
    assert_eq!(
        discovery.sessions[0].key.effective_root,
        root.effective_root
    );
}

#[test]
fn alternate_and_default_roots_yield_distinct_identities() {
    let home = tempfile::tempdir().unwrap();
    let custom = tempfile::tempdir().unwrap();

    let default_root = claude::resolve_root(None, Some(home.path())).unwrap();
    let alt_root = claude::resolve_root(Some(custom.path().as_os_str()), None).unwrap();

    for root in [&default_root, &alt_root] {
        let cwd = root.effective_root.join("work");
        fs::create_dir_all(&cwd).unwrap();
        write_transcript(
            &root.effective_root,
            "key",
            UUID_A,
            &standard_records(UUID_A, cwd.to_str().unwrap(), "hi"),
        );
    }

    let a = claude::discover(&default_root).unwrap();
    let b = claude::discover(&alt_root).unwrap();
    assert_eq!(a.sessions.len(), 1);
    assert_eq!(b.sessions.len(), 1);
    // Same UUID but different effective root → distinct keys.
    assert_ne!(a.sessions[0].key, b.sessions[0].key);
}

// ===========================================================================
// Fixture: missing Workspace
// ===========================================================================

#[test]
fn missing_workspace_yields_unknown_evidence() {
    let home = tempfile::tempdir().unwrap();
    let root = claude::resolve_root(None, Some(home.path())).unwrap();

    // sessionId agrees with filename, but no cwd anywhere.
    write_transcript(
        &default_root_dir(home.path()),
        "key",
        UUID_A,
        &[json(&[
            ("type", str_val("user")),
            ("sessionId", str_val(UUID_A)),
            (
                "message",
                json(&[("role", str_val("user")), ("content", str_val("hi"))]),
            ),
        ])],
    );

    let discovery = claude::discover(&root).unwrap();
    assert_eq!(
        discovery.sessions.len(),
        1,
        "supported ID still discoverable"
    );
    assert_eq!(discovery.sessions[0].workspace, WorkspaceEvidence::Unknown);
    assert_eq!(discovery.sessions[0].support, SupportStatus::Supported);
}

// ===========================================================================
// Fixture: fake claude launch contract — captures exact cwd/argv/env
// ===========================================================================

/// A fake `claude` launcher that records the exact cwd, argv, and environment
/// it would exec with, then exits successfully without spawning anything.
#[test]
fn resolve_root_prefers_explicit_config_dir() {
    let explicit = PathBuf::from("/custom/claude");
    let home = PathBuf::from("/home/user");
    let root = claude::resolve_root(Some(explicit.as_os_str()), Some(&home)).unwrap();
    assert_eq!(root.effective_root, explicit);
    assert!(root.nondefault);
}

#[test]
fn resolve_root_falls_back_to_home_dot_claude() {
    let home = PathBuf::from("/home/user");
    let root = claude::resolve_root(None, Some(&home)).unwrap();
    assert_eq!(root.effective_root, PathBuf::from("/home/user/.claude"));
    assert!(!root.nondefault);
}

#[test]
fn resolve_root_returns_none_without_env_or_home() {
    assert!(claude::resolve_root(None, None).is_none());
}

#[test]
fn empty_config_dir_env_falls_back_to_home() {
    let home = PathBuf::from("/home/user");
    let root = claude::resolve_root(Some(OsStr::new("")), Some(&home)).unwrap();
    assert_eq!(root.effective_root, PathBuf::from("/home/user/.claude"));
    assert!(!root.nondefault);
}

// ===========================================================================
// UUID helper unit checks
// ===========================================================================
