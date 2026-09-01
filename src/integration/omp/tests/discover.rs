use super::*;

// ===========================================================================
// DUPLICATE IDs ACROSS PROFILES → distinct sessions (highest isolation risk)
// ===========================================================================

#[test]
fn same_id_across_default_and_named_profile_are_distinct() {
    let fx = Fixture::new();
    // Default profile session.
    let default_path = fx.write_flat(
        &fx.default_agent_root,
        "default.jsonl",
        &[
            header_v3("dup-id", &fx.workspace, 1700000000),
            user_message_string("default profile", 1700000010),
        ],
    );
    // Named profile session with the SAME id.
    let named_root = fx.profile_agent_root("work");
    let named_path = fx.write_flat(
        &named_root,
        "named.jsonl",
        &[
            header_v3("dup-id", &fx.workspace, 1700000000),
            user_message_string("work profile", 1700000010),
        ],
    );

    let scope = fx.scope_exact_workspace();
    let cfg_d = DiscoverConfig::new(fx.roots_default(), &scope);
    let cfg_n = DiscoverConfig::new(fx.roots_named("work"), &scope);
    let od = omp::discover(&cfg_d).unwrap();
    let on = omp::discover(&cfg_n).unwrap();

    assert_eq!(od.parsed.len(), 1);
    assert_eq!(on.parsed.len(), 1);
    // Same id but distinct locators AND distinct profile identity.
    assert_eq!(od.parsed[0].id, on.parsed[0].id);
    assert_ne!(od.parsed[0].transcript_path, on.parsed[0].transcript_path);

    // The SessionKeys must differ on profile, so they cannot collide.
    let sk_d = od.parsed[0].clone().into_session(
        &fx.roots_default(),
        crate::session::RiskStatus::Normal,
        crate::session::ActivityStatus::Unknown,
    );
    let sk_n = on.parsed[0].clone().into_session(
        &fx.roots_named("work"),
        crate::session::RiskStatus::Normal,
        crate::session::ActivityStatus::Unknown,
    );
    assert_ne!(sk_d.key, sk_n.key, "profile is part of identity");
    assert_eq!(sk_d.key.profile, None);
    assert_eq!(
        sk_n.key.profile.as_deref(),
        Some(std::ffi::OsStr::new("work"))
    );
    let _ = (default_path, named_path);
}

#[test]
fn same_id_across_two_named_profiles_are_distinct() {
    let fx = Fixture::new();
    let root_a = fx.profile_agent_root("alpha");
    let root_b = fx.profile_agent_root("beta");
    fx.write_flat(
        &root_a,
        "a.jsonl",
        &[
            header_v3("shared", &fx.workspace, 1700000000),
            user_message_string("alpha", 1700000010),
        ],
    );
    fx.write_flat(
        &root_b,
        "b.jsonl",
        &[
            header_v3("shared", &fx.workspace, 1700000000),
            user_message_string("beta", 1700000010),
        ],
    );

    let scope = fx.scope_exact_workspace();
    let ca = DiscoverConfig::new(fx.roots_named("alpha"), &scope);
    let cb = DiscoverConfig::new(fx.roots_named("beta"), &scope);
    let oa = omp::discover(&ca).unwrap();
    let ob = omp::discover(&cb).unwrap();
    assert_eq!(oa.parsed.len(), 1);
    assert_eq!(ob.parsed.len(), 1);

    let sa = oa.parsed[0].clone().into_session(
        &fx.roots_named("alpha"),
        crate::session::RiskStatus::Normal,
        crate::session::ActivityStatus::Unknown,
    );
    let sb = ob.parsed[0].clone().into_session(
        &fx.roots_named("beta"),
        crate::session::RiskStatus::Normal,
        crate::session::ActivityStatus::Unknown,
    );
    assert_ne!(sa.key, sb.key);
}

#[test]
fn duplicate_files_within_same_profile_root_are_deduped() {
    let fx = Fixture::new();
    let path = fx.write(
        &fx.default_agent_root,
        &fx.encoded_ws(),
        "dup.jsonl",
        &[
            header_v3("dup", &fx.workspace, 1700000000),
            user_message_string("x", 1700000010),
        ],
    );
    #[cfg(unix)]
    {
        let link = fx
            .default_agent_root
            .join(fx.encoded_ws())
            .join("dup-link.jsonl");
        std::os::unix::fs::symlink(&path, &link).unwrap();
    }
    let outcome = fx.discover(fx.roots_default());
    #[cfg(unix)]
    {
        assert_eq!(outcome.parsed.len(), 1);
        assert_eq!(outcome.skipped_files, 0, "symlink was read, not skipped");
    }
    #[cfg(not(unix))]
    assert_eq!(outcome.parsed.len(), 1);
}

#[cfg(unix)]
#[test]
fn symlinked_session_inside_effective_root_is_read() {
    let fx = Fixture::new();
    let target = fx.default_agent_root.join("target.data");
    fx.write_jsonl(
        &target,
        &[
            header_v3("inside-link", &fx.workspace, 1700000000),
            user_message_string("followed safely", 1700000010),
        ],
    );
    let link_dir = fx.default_agent_root.join(fx.encoded_ws());
    fs::create_dir_all(&link_dir).unwrap();
    std::os::unix::fs::symlink(&target, link_dir.join("inside.jsonl")).unwrap();

    let outcome = fx.discover(fx.roots_default());
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
    let link_dir = fx.default_agent_root.join(fx.encoded_ws());
    fs::create_dir_all(&link_dir).unwrap();
    std::os::unix::fs::symlink(&target, link_dir.join("outside.jsonl")).unwrap();

    let outcome = fx.discover(fx.roots_default());
    assert!(outcome.parsed.is_empty());
    assert_eq!(outcome.skipped_files, 1, "rejection must be diagnosed");
}

// ===========================================================================
// SCOPE FILTERING
// ===========================================================================

#[test]
fn out_of_scope_workspace_excluded() {
    let fx = Fixture::new();
    let other_ws = fx.home().join("other-ws");
    fs::create_dir_all(&other_ws).unwrap();
    fx.write_flat(
        &fx.default_agent_root,
        "other.jsonl",
        &[
            header_v3("other", &other_ws, 1700000000),
            user_message_string("other", 1700000010),
        ],
    );
    let outcome = fx.discover(fx.roots_default());
    assert_eq!(outcome.parsed.len(), 0);
    assert_eq!(outcome.out_of_scope, 1);
}

#[test]
fn grouped_directory_overrides_migrated_header_cwd() {
    let fx = Fixture::new();
    let mac_workspace = Path::new("/Users/luwei.will/workspace");
    fx.write(
        &fx.default_agent_root,
        &fx.encoded_ws(),
        "migrated.jsonl",
        &[
            header_v3("migrated", mac_workspace, 1700000000),
            user_message_string("migrated", 1700000010),
        ],
    );

    let outcome = fx.discover(fx.roots_default());
    assert_eq!(outcome.parsed.len(), 1);
    assert_eq!(outcome.out_of_scope, 0);
}
#[test]
fn out_of_scope_grouped_directory_is_pruned_without_reading_files() {
    let fx = Fixture::new();
    let other_ws = fx.home().join("other-ws");
    fs::create_dir_all(&other_ws).unwrap();
    let encoded_other = format!("-{}-", other_ws.display().to_string().replace('/', "-"));
    let dir = fx.default_agent_root.join(&encoded_other);
    fs::create_dir_all(&dir).unwrap();
    // Garbage contents: pruning must skip the read entirely (a read would
    // surface as no_header_files/skipped_files).
    fs::write(dir.join("garbage.jsonl"), b"not json at all\n").unwrap();

    let outcome = fx.discover(fx.roots_default());
    assert_eq!(outcome.parsed.len(), 0);
    assert_eq!(outcome.pruned_dirs, 1);
    assert_eq!(outcome.no_header_files, 0, "pruned dir must not be read");
    assert_eq!(outcome.skipped_files, 0);
}

#[test]
fn home_relative_grouped_directory_is_kept() {
    let fx = Fixture::new();
    // OMP's real encoding for a workspace under $HOME is home-relative:
    // `<home>/workspace` -> `-workspace`. The prefilter must keep it when
    // given the fixture home.
    let relative = fx
        .workspace
        .strip_prefix(fx.home())
        .unwrap()
        .display()
        .to_string();
    let dir_name = format!("-{}", relative.replace('/', "-"));
    fx.write(
        &fx.default_agent_root,
        &dir_name,
        "rel.jsonl",
        &[
            header_v3("rel", &fx.workspace, 1700000000),
            user_message_string("rel", 1700000010),
        ],
    );
    let scope = fx.scope_exact_workspace();
    let cfg = DiscoverConfig::new(fx.roots_default(), &scope).with_home(Some(fx.home()));
    let outcome = omp::discover(&cfg).unwrap();
    assert_eq!(outcome.parsed.len(), 1);
    assert_eq!(outcome.pruned_dirs, 0);
}

#[cfg(unix)]
#[test]
fn home_relative_grouped_directory_is_kept_when_home_is_a_symlink() {
    let fx = Fixture::new();
    let symlink_home = fx.home().join("home-link");
    std::os::unix::fs::symlink(fx.home(), &symlink_home).unwrap();
    fx.write(
        &fx.default_agent_root,
        "-workspace",
        "rel-symlink-home.jsonl",
        &[
            header_v3("rel-symlink-home", &fx.workspace, 1700000000),
            user_message_string("rel", 1700000010),
        ],
    );
    let scope = fx.scope_exact_workspace();
    let cfg = DiscoverConfig::new(fx.roots_default(), &scope).with_home(Some(symlink_home));

    let outcome = omp::discover(&cfg).unwrap();

    assert_eq!(outcome.parsed.len(), 1);
    assert_eq!(outcome.pruned_dirs, 0);
}

#[test]
fn custom_session_dir_filters_by_header_cwd_not_directory() {
    let fx = Fixture::new();
    let custom = fx.home().join("custom-sessions");
    fs::create_dir_all(&custom).unwrap();
    let roots = fx.roots_custom(custom.clone(), ProfileSelection::Default);
    // Directory name does not encode the workspace.
    fx.write_flat(
        &custom,
        "misc.jsonl",
        &[
            header_v3("flat", &fx.workspace, 1700000000),
            user_message_string("flat", 1700000010),
        ],
    );
    let outcome = fx.discover(roots);
    assert_eq!(outcome.parsed.len(), 1);
}

#[test]
fn down_scope_includes_descendant_workspaces() {
    let fx = Fixture::new();
    let child_ws = fx.workspace.join("subdir");
    fs::create_dir_all(&child_ws).unwrap();
    fx.write(
        &fx.default_agent_root,
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
    let outcome = omp::discover(&cfg).unwrap();
    assert_eq!(outcome.parsed.len(), 1);
}

// ===========================================================================
// MALFORMED / TRUNCATED / EMPTY / MISSING WORKSPACE
// ===========================================================================

#[test]
fn malformed_middle_record_does_not_abort_discovery() {
    let fx = Fixture::new();
    let path = fx
        .default_agent_root
        .join(fx.encoded_ws())
        .join("mid.jsonl");
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
    let outcome = fx.discover(fx.roots_default());
    assert_eq!(outcome.parsed.len(), 1);
    assert_eq!(outcome.parsed[0].messages.len(), 1);
    assert_eq!(outcome.parsed[0].messages[0].text, "after");
}

#[test]
fn truncated_tail_keeps_valid_records() {
    let fx = Fixture::new();
    let path = fx
        .default_agent_root
        .join(fx.encoded_ws())
        .join("trunc.jsonl");
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
    let outcome = fx.discover(fx.roots_default());
    assert_eq!(outcome.parsed.len(), 1);
    assert_eq!(outcome.parsed[0].id, "trunc");
    assert!(outcome.parsed[0].messages.is_empty());
}

#[test]
fn empty_file_is_no_header() {
    let fx = Fixture::new();
    fs::File::create(fx.default_agent_root.join("empty.jsonl")).unwrap();
    let outcome = fx.discover(fx.roots_default());
    assert_eq!(outcome.parsed.len(), 0);
    assert_eq!(outcome.no_header_files, 1);
}

#[test]
fn missing_workspace_is_discoverable_but_unresumable_cwd() {
    let fx = Fixture::new();
    let header = json!({ "type": "session", "id": "nws", "timestamp": 1700000000u64 });
    fx.write(
        &fx.default_agent_root,
        &fx.encoded_ws(),
        "nws.jsonl",
        &[header, user_message_string("hi", 1700000010)],
    );
    let outcome = fx.discover(fx.roots_default());
    assert_eq!(outcome.parsed.len(), 1);
    assert!(outcome.parsed[0].workspace.is_none());
}

// ===========================================================================
// READ-ONLY INVARIANT: discovery never modifies files
// ===========================================================================

#[test]
fn discovery_does_not_modify_files_bytes_or_mtimes() {
    let fx = Fixture::new();
    let path = fx.write(
        &fx.default_agent_root,
        &fx.encoded_ws(),
        "ro.jsonl",
        &[
            title_sidecar("RO"),
            header_v3("ro", &fx.workspace, 1700000000),
            user_message_string("x", 1700000010),
        ],
    );
    let before_dir = snapshot::snapshot_dir(&fx.base_root, true).unwrap();
    let before_file = snapshot::snapshot_file(&path).unwrap();

    let _ = fx.discover(fx.roots_default());

    let after_dir = snapshot::snapshot_dir(&fx.base_root, true).unwrap();
    snapshot::assert_unchanged(&before_dir, &after_dir);
    let after_file = snapshot::snapshot_file(&path).unwrap();
    snapshot::assert_file_unchanged(&before_file, &after_file);
}

#[test]
fn discovery_of_growing_file_is_read_only() {
    let fx = Fixture::new();
    let path = fx.write(
        &fx.default_agent_root,
        &fx.encoded_ws(),
        "live.jsonl",
        &[
            header_v3("live", &fx.workspace, 1700000000),
            user_message_string("first", 1700000010),
        ],
    );
    fx.append_raw(
        &path,
        b"{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"being",
    );
    let before = snapshot::snapshot_file(&path).unwrap();
    let outcome = fx.discover(fx.roots_default());
    let after = snapshot::snapshot_file(&path).unwrap();
    snapshot::assert_file_unchanged(&before, &after);
    assert_eq!(outcome.parsed.len(), 1);
}

#[test]
fn discovery_does_not_create_or_migrate_any_files() {
    let fx = Fixture::new();
    fx.write(
        &fx.default_agent_root,
        &fx.encoded_ws(),
        "a.jsonl",
        &[
            header_v3("a", &fx.workspace, 1700000000),
            user_message_string("a", 1700000010),
        ],
    );
    let before = snapshot::snapshot_dir(&fx.base_root, true).unwrap();
    let _ = fx.discover(fx.roots_default());
    let after = snapshot::snapshot_dir(&fx.base_root, true).unwrap();
    snapshot::assert_unchanged(&before, &after);
}
