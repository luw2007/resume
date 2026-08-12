#![allow(unused_imports)]
use crate::integration::pi::test_support::*;
use crate::{
    integration::pi::{
        self, DiscoverConfig, EffectiveRoots, ParsedSession, ResolutionInputs,
        SessionControlEvidence,
    },
    preview::snapshot,
    scope::{Direction, Scope},
};
use serde_json::json;
use std::{
    ffi::OsString,
    fs,
    io::Write,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};
#[test]
fn custom_flat_root_filters_by_header_cwd_not_directory_name() {
    let fx = Fixture::new();
    let custom = fx.agent_root.join("flat-sessions");
    fs::create_dir_all(&custom).unwrap();
    let roots = fx.roots_custom(custom.clone());

    // A file whose directory name does NOT encode the workspace; only header
    // cwd determines membership.
    let path = custom.join("misc.jsonl");
    let mut file = fs::File::create(&path).unwrap();
    writeln!(
        file,
        "{}",
        serde_json::to_string(&header_v3("flat-id", &fx.workspace, 1700000000)).unwrap()
    )
    .unwrap();
    writeln!(
        file,
        "{}",
        serde_json::to_string(&user_message_string("flat", 1700000010)).unwrap()
    )
    .unwrap();

    let outcome = fx.discover_custom(roots.clone());
    assert_eq!(outcome.parsed.len(), 1, "header cwd must match via Scope");
}

#[test]
fn out_of_scope_workspace_is_excluded() {
    let fx = Fixture::new();
    let other_ws = fx.home().join("other-workspace");
    fs::create_dir_all(&other_ws).unwrap();
    fx.write_grouped(
        &fx.encoded_ws(),
        "other.jsonl",
        &[
            header_v3("other", &other_ws, 1700000000),
            user_message_string("other", 1700000010),
        ],
    );
    let outcome = fx.discover_default();
    assert_eq!(outcome.parsed.len(), 0);
    assert_eq!(outcome.out_of_scope, 1);
}
#[test]
fn out_of_scope_grouped_directory_is_pruned_without_reading_files() {
    let fx = Fixture::new();
    let other_ws = fx.home().join("other-workspace");
    fs::create_dir_all(&other_ws).unwrap();
    // Directory named with the real encoding of an out-of-scope workspace.
    // Contents are deliberately garbage: pruning must skip the read entirely
    // (a read would surface as no_header_files/skipped_files).
    let encoded_other = format!("-{}-", other_ws.display().to_string().replace('/', "-"));
    let dir = fx.session_root.join(&encoded_other);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("garbage.jsonl"), b"not json at all\n").unwrap();

    let outcome = fx.discover_default();
    assert_eq!(outcome.parsed.len(), 0);
    assert_eq!(outcome.pruned_dirs, 1);
    assert_eq!(outcome.no_header_files, 0, "pruned dir must not be read");
    assert_eq!(outcome.skipped_files, 0);
}

#[test]
fn custom_session_root_directories_are_never_pruned() {
    let fx = Fixture::new();
    let custom = fx.agent_root.join("flat-sessions");
    // Custom layout: directory names carry no workspace encoding; an
    // arbitrary name must still be scanned and its header cwd honored.
    let dir = custom.join("opaque-subdir");
    fs::create_dir_all(&dir).unwrap();
    let mut file = fs::File::create(dir.join("s.jsonl")).unwrap();
    writeln!(
        file,
        "{}",
        serde_json::to_string(&header_v3("s", &fx.workspace, 1700000000)).unwrap()
    )
    .unwrap();

    let outcome = fx.discover_custom(fx.roots_custom(custom));
    assert_eq!(outcome.parsed.len(), 1);
    assert_eq!(outcome.pruned_dirs, 0);
}

#[test]
fn down_scope_includes_descendant_workspaces() {
    let fx = Fixture::new();
    let child_ws = fx.workspace.join("subdir");
    fs::create_dir_all(&child_ws).unwrap();
    fx.write_grouped(
        &fx.encoded_ws(),
        "child.jsonl",
        &[
            header_v3("child", &child_ws, 1700000000),
            user_message_string("child", 1700000010),
        ],
    );
    let scope = Scope::new(
        fx.workspace.canonicalize().unwrap(),
        Some(Direction::Down(crate::cli::Distance::Finite(2))),
        crate::scope::DefaultScope::Exact { git_warning: None },
    );
    let cfg = DiscoverConfig::new(fx.roots_default(), &scope);
    let outcome = pi::discover(&cfg).unwrap();
    assert_eq!(outcome.parsed.len(), 1);
}

// ---------------------------------------------------------------------------
// Dedupe keyed by effective session root + canonical transcript locator
// ---------------------------------------------------------------------------

#[test]
fn duplicate_files_under_same_root_are_deduped() {
    let fx = Fixture::new();
    let path = fx.write_grouped(
        &fx.encoded_ws(),
        "dup.jsonl",
        &[
            header_v3("dup-id", &fx.workspace, 1700000000),
            user_message_string("dup", 1700000010),
        ],
    );
    // Symlink the same file to a second path; canonical locator is identical.
    #[cfg(unix)]
    {
        let link = fx.session_root.join(&fx.encoded_ws()).join("dup-link.jsonl");
        std::os::unix::fs::symlink(&path, &link).unwrap();
    }
    let outcome = fx.discover_default();
    #[cfg(unix)]
    {
        assert_eq!(outcome.parsed.len(), 1, "dedupe by canonical locator");
        assert_eq!(outcome.skipped_files, 0, "symlink was read, not skipped");
    }
    #[cfg(not(unix))]
    assert_eq!(outcome.parsed.len(), 1);
}

#[cfg(unix)]
#[test]
fn symlinked_session_inside_effective_root_is_read() {
    let fx = Fixture::new();
    let target = fx.session_root.join("target.data");
    fx.write_jsonl(
        &target,
        &[
            header_v3("inside-link", &fx.workspace, 1700000000),
            user_message_string("followed safely", 1700000010),
        ],
    );
    let link_dir = fx.session_root.join(&fx.encoded_ws());
    fs::create_dir_all(&link_dir).unwrap();
    std::os::unix::fs::symlink(&target, link_dir.join("inside.jsonl")).unwrap();

    let outcome = fx.discover_default();
    assert_eq!(outcome.parsed.len(), 1);
    assert_eq!(outcome.parsed[0].id, "inside-link");
    assert_eq!(outcome.skipped_files, 0);
}

#[cfg(unix)]
#[test]
fn symlinked_session_outside_effective_root_is_rejected_with_diagnostic_count() {
    let fx = Fixture::new();
    let outside = tempfile::tempdir().unwrap();
    let target = outside.path().join("foreign.data");
    fx.write_jsonl(
        &target,
        &[
            header_v3("outside-link", &fx.workspace, 1700000000),
            user_message_string("must not leak", 1700000010),
        ],
    );
    let link_dir = fx.session_root.join(&fx.encoded_ws());
    fs::create_dir_all(&link_dir).unwrap();
    std::os::unix::fs::symlink(&target, link_dir.join("outside.jsonl")).unwrap();

    let outcome = fx.discover_default();
    assert!(outcome.parsed.is_empty());
    assert_eq!(outcome.skipped_files, 1, "rejection must be diagnosed");
}

#[test]
fn same_id_across_different_roots_are_distinct() {
    // Two session roots, same header id, distinct transcript locators.
    let fx = Fixture::new();
    let root_a = fx.agent_root.join("rootA/sessions");
    let root_b = fx.agent_root.join("rootB/sessions");
    for root in [&root_a, &root_b] {
        fs::create_dir_all(root).unwrap();
    }

    let write = |root: &Path, name: &str| {
        let mut file = fs::File::create(root.join(name)).unwrap();
        writeln!(
            file,
            "{}",
            serde_json::to_string(&header_v3("shared-id", &fx.workspace, 1700000000)).unwrap()
        )
        .unwrap();
        writeln!(
            file,
            "{}",
            serde_json::to_string(&user_message_string("x", 1700000010)).unwrap()
        )
        .unwrap();
    };
    write(&root_a, "a.jsonl");
    write(&root_b, "b.jsonl");

    let roots_a = EffectiveRoots {
        agent_root: fx.agent_root.clone(),
        session_root: root_a.clone(),
        custom_session_root: true,
    };
    let roots_b = EffectiveRoots {
        agent_root: fx.agent_root.clone(),
        session_root: root_b.clone(),
        custom_session_root: true,
    };

    let scope = fx.scope_exact_workspace();
    let cfg_a = DiscoverConfig::new(roots_a.clone(), &scope);
    let cfg_b = DiscoverConfig::new(roots_b.clone(), &scope);
    let oa = pi::discover(&cfg_a).unwrap();
    let ob = pi::discover(&cfg_b).unwrap();
    assert_eq!(oa.parsed.len(), 1);
    assert_eq!(ob.parsed.len(), 1);
    // They have the same id but distinct locators/roots → different SessionKey.
    assert_ne!(oa.parsed[0].transcript_path, ob.parsed[0].transcript_path);
}

// ---------------------------------------------------------------------------
// Timestamp fallback chain
// ---------------------------------------------------------------------------

#[test]
fn activity_time_prefers_message_then_header_then_mtime() {
    let fx = Fixture::new();
    let path = fx.write_grouped(
        &fx.encoded_ws(),
        "ts.jsonl",
        &[
            header_v3("ts", &fx.workspace, 1700000000),
            user_message_string("hi", 1700000050),
        ],
    );
    let outcome = fx.discover_default();
    let parsed = &outcome.parsed[0];
    let expected = SystemTime::UNIX_EPOCH + Duration::from_secs(1700000050);
    assert_eq!(parsed.activity_time, Some(expected));

    let bounds = crate::preview::jsonl::Bounds::default();

    // Now a file with no messages: falls back to header time.
    let path2 = fx.write_grouped(
        &fx.encoded_ws(),
        "header-only.jsonl",
        &[header_v3("ho", &fx.workspace, 1700000000)],
    );
    let result2 = crate::preview::jsonl::read_file(&path2, &bounds).unwrap();
    let parsed2 = pi::extract_session_pub(&path2, &result2, None).unwrap();
    assert_eq!(
        parsed2.activity_time,
        Some(SystemTime::UNIX_EPOCH + Duration::from_secs(1700000000))
    );

    // File with no header timestamp: falls back to mtime.
    let header_no_ts = json!({ "type": "session", "id": "nts", "cwd": fx.workspace });
    let path3 = fx.write_grouped(&fx.encoded_ws(), "no-ts.jsonl", &[header_no_ts]);
    let mtime = fs::metadata(&path3).unwrap().modified().unwrap();
    let result3 = crate::preview::jsonl::read_file(&path3, &bounds).unwrap();
    let parsed3 = pi::extract_session_pub(&path3, &result3, Some(mtime)).unwrap();
    assert_eq!(parsed3.activity_time, Some(mtime));

    let _ = path;
}

// ---------------------------------------------------------------------------
// Malformed middle/tail records and missing Workspace
// ---------------------------------------------------------------------------

#[test]
fn malformed_middle_record_does_not_abort_discovery() {
    let fx = Fixture::new();
    let path = fx.session_root.join(&fx.encoded_ws()).join("mid.jsonl");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut file = fs::File::create(&path).unwrap();
    writeln!(
        file,
        "{}",
        serde_json::to_string(&header_v3("mid", &fx.workspace, 1700000000)).unwrap()
    )
    .unwrap();
    writeln!(file, "{{ this is not valid json }}").unwrap();
    writeln!(
        file,
        "{}",
        serde_json::to_string(&user_message_string("after", 1700000010)).unwrap()
    )
    .unwrap();

    let outcome = fx.discover_default();
    assert_eq!(outcome.parsed.len(), 1);
    assert_eq!(outcome.parsed[0].messages.len(), 1);
    assert_eq!(outcome.parsed[0].messages[0].text, "after");
}

#[test]
fn truncated_tail_is_incomplete_but_keeps_valid_records() {
    let fx = Fixture::new();
    let path = fx.session_root.join(&fx.encoded_ws()).join("trunc.jsonl");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut file = fs::File::create(&path).unwrap();
    writeln!(
        file,
        "{}",
        serde_json::to_string(&header_v3("trunc", &fx.workspace, 1700000000)).unwrap()
    )
    .unwrap();
    write!(
        file,
        "{{\"type\":\"user\",\"message\":{{\"role\":\"user\",\"content\":\"being writ"
    )
    .unwrap();
    file.sync_all().unwrap();

    let outcome = fx.discover_default();
    // Header is valid; the incomplete tail is dropped.
    assert_eq!(outcome.parsed.len(), 1);
    assert_eq!(outcome.parsed[0].id, "trunc");
    assert!(outcome.parsed[0].messages.is_empty());
}

#[test]
fn missing_workspace_is_discoverable_but_unresumable_cwd() {
    let fx = Fixture::new();
    let header = json!({ "type": "session", "id": "nws", "timestamp": 1700000000u64 });
    fx.write_grouped(
        &fx.encoded_ws(),
        "no-ws.jsonl",
        &[header, user_message_string("hi", 1700000010)],
    );
    let outcome = fx.discover_default();
    // Missing Workspace: surfaced for diagnosis (not out-of-scope, not excluded).
    assert_eq!(outcome.parsed.len(), 1);
    assert!(outcome.parsed[0].workspace.is_none());
}

// ---------------------------------------------------------------------------
// ResumeSpec generation
// ---------------------------------------------------------------------------

#[test]
fn discovery_does_not_modify_v1_v2_v3_files_bytes_or_mtimes() {
    let fx = Fixture::new();

    let v1_path = fx.write_grouped(
        &fx.encoded_ws(),
        "v1.jsonl",
        &[
            header_v1("v1", &fx.workspace, 1700000000),
            user_message_string("v1 msg", 1700000010),
        ],
    );
    let v2_path = fx.write_grouped(
        &fx.encoded_ws(),
        "v2.jsonl",
        &[
            header_v2("v2", &fx.workspace, 1700000000),
            user_message_string("v2 msg", 1700000010),
        ],
    );
    let v3_path = fx.write_grouped(
        &fx.encoded_ws(),
        "v3.jsonl",
        &[
            header_v3("v3", &fx.workspace, 1700000000),
            user_message_string("v3 msg", 1700000010),
        ],
    );

    let before = snapshot::snapshot_dir(&fx.agent_root, true).unwrap();
    let snaps_before: Vec<_> = [&v1_path, &v2_path, &v3_path]
        .iter()
        .map(|p| snapshot::snapshot_file(p).unwrap())
        .collect();

    // Run discovery.
    let _ = fx.discover_default();

    let after = snapshot::snapshot_dir(&fx.agent_root, true).unwrap();
    snapshot::assert_unchanged(&before, &after);

    for (path, snap_before) in [&v1_path, &v2_path, &v3_path].iter().zip(&snaps_before) {
        let snap_after = snapshot::snapshot_file(path).unwrap();
        snapshot::assert_file_unchanged(snap_before, &snap_after);
    }
}

#[test]
fn discovery_of_growing_file_is_read_only() {
    let fx = Fixture::new();
    let path = fx.write_grouped(
        &fx.encoded_ws(),
        "live.jsonl",
        &[
            header_v3("live", &fx.workspace, 1700000000),
            user_message_string("first", 1700000010),
        ],
    );
    // Simulate a live writer appending a partial record.
    fx.append_raw(
        &path,
        b"{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"being",
    );

    let before = snapshot::snapshot_file(&path).unwrap();
    let outcome = fx.discover_default();
    let after = snapshot::snapshot_file(&path).unwrap();
    snapshot::assert_file_unchanged(&before, &after);
    assert_eq!(outcome.parsed.len(), 1);
}

#[test]
fn discovery_does_not_create_or_migrate_any_files() {
    let fx = Fixture::new();
    fx.write_grouped(
        &fx.encoded_ws(),
        "a.jsonl",
        &[
            header_v3("a", &fx.workspace, 1700000000),
            user_message_string("a", 1700000010),
        ],
    );
    let before = snapshot::snapshot_dir(&fx.agent_root, true).unwrap();
    let _ = fx.discover_default();
    let after = snapshot::snapshot_dir(&fx.agent_root, true).unwrap();
    snapshot::assert_unchanged(&before, &after);
}

// ---------------------------------------------------------------------------
// Fake `pi` executable capturing exact cwd/argv/env
