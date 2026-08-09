use super::*;

// ===========================================================================
// ROOT RESOLUTION: base, profile, environment interactions
// ===========================================================================

#[test]
fn resolves_default_agent_root_from_home() {
    let fx = Fixture::new();
    let roots = omp::resolve(&fx.inputs_default()).expect("home present");
    assert_eq!(roots.config_root, fx.base_root);
    assert_eq!(roots.agent_root, fx.default_agent_root);
    assert_eq!(roots.session_root, fx.default_agent_root);
    assert!(!roots.custom_session_root);
    assert_eq!(roots.profile, ProfileSelection::Default);
}

#[test]
fn config_dir_env_overrides_base() {
    let tmp = tempfile::tempdir().unwrap();
    let custom_base = tmp.path().join("custom-omp");
    fs::create_dir_all(custom_base.join("agent")).unwrap();
    let inputs = ResolutionInputs {
        home: Some(tmp.path().to_path_buf()),
        config_dir_env: Some(custom_base.clone()),
        ..Default::default()
    };
    let roots = omp::resolve(&inputs).unwrap();
    assert_eq!(roots.config_root, custom_base);
    assert_eq!(roots.agent_root, custom_base.join("agent"));
}

#[test]
fn agent_dir_env_overrides_unprofiled_agent_root_only() {
    let fx = Fixture::new();
    let custom_agent = fx.home().join("alt-agent");
    fs::create_dir_all(&custom_agent).unwrap();
    let inputs = ResolutionInputs {
        agent_dir_env: Some(custom_agent.clone()),
        ..fx.inputs_default()
    };
    let roots = omp::resolve(&inputs).unwrap();
    assert_eq!(roots.agent_root, custom_agent);
    assert_eq!(roots.session_root, custom_agent);
}

#[test]
fn named_profile_ignores_agent_dir_env() {
    // KEY INVARIANT: named profile selection deliberately ignores the
    // unprofiled PI_CODING_AGENT_DIR. This is the highest isolation-risk path.
    let fx = Fixture::new();
    let decoy_agent = fx.home().join("decoy-agent");
    fs::create_dir_all(&decoy_agent).unwrap();
    let inputs = ResolutionInputs {
        agent_dir_env: Some(decoy_agent.clone()),
        profile_flag: Some(OsString::from("work")),
        ..fx.inputs_default()
    };
    let roots = omp::resolve(&inputs).unwrap();
    // Named profile uses <base>/profiles/work/agent, NOT the decoy.
    assert_eq!(
        roots.agent_root,
        fx.base_root.join("profiles").join("work").join("agent")
    );
    assert_ne!(roots.agent_root, decoy_agent);
    assert_eq!(
        roots.profile,
        ProfileSelection::Named(OsString::from("work"))
    );
}

#[test]
fn profile_flag_beats_omp_and_pi_profile_env() {
    let fx = Fixture::new();
    let inputs = ResolutionInputs {
        profile_flag: Some(OsString::from("flag")),
        omp_profile_env: Some(OsString::from("omp-env")),
        pi_profile_env: Some(OsString::from("pi-env")),
        ..fx.inputs_default()
    };
    let roots = omp::resolve(&inputs).unwrap();
    assert_eq!(
        roots.profile,
        ProfileSelection::Named(OsString::from("flag"))
    );
}

#[test]
fn omp_profile_beats_pi_profile() {
    let fx = Fixture::new();
    let inputs = ResolutionInputs {
        omp_profile_env: Some(OsString::from("omp-env")),
        pi_profile_env: Some(OsString::from("pi-env")),
        ..fx.inputs_default()
    };
    let roots = omp::resolve(&inputs).unwrap();
    assert_eq!(
        roots.profile,
        ProfileSelection::Named(OsString::from("omp-env"))
    );
}

#[test]
fn pi_profile_selected_when_no_omp_profile_or_flag() {
    let fx = Fixture::new();
    let inputs = ResolutionInputs {
        pi_profile_env: Some(OsString::from("pi-env")),
        ..fx.inputs_default()
    };
    let roots = omp::resolve(&inputs).unwrap();
    assert_eq!(
        roots.profile,
        ProfileSelection::Named(OsString::from("pi-env"))
    );
}

#[test]
fn session_dir_flag_overrides_session_root() {
    let fx = Fixture::new();
    let custom_sessions = fx.home().join("alt-sessions");
    fs::create_dir_all(&custom_sessions).unwrap();
    let inputs = ResolutionInputs {
        session_dir_flag: Some(custom_sessions.clone()),
        ..fx.inputs_default()
    };
    let roots = omp::resolve(&inputs).unwrap();
    assert!(roots.custom_session_root);
    assert_eq!(roots.session_root, custom_sessions);
}

#[test]
fn xdg_data_home_overrides_default_profile_data_root() {
    let fx = Fixture::new();
    let xdg = fx.home().join("xdg-data");
    fs::create_dir_all(&xdg).unwrap();
    let inputs = ResolutionInputs {
        xdg_data_home: Some(xdg.clone()),
        ..fx.inputs_default()
    };
    let roots = omp::resolve(&inputs).unwrap();
    assert_eq!(roots.agent_root, xdg.join("agent"));
}

#[test]
fn xdg_data_home_ignored_for_named_profiles() {
    let fx = Fixture::new();
    let xdg = fx.home().join("xdg-data");
    fs::create_dir_all(&xdg).unwrap();
    let inputs = ResolutionInputs {
        xdg_data_home: Some(xdg.clone()),
        profile_flag: Some(OsString::from("work")),
        ..fx.inputs_default()
    };
    let roots = omp::resolve(&inputs).unwrap();
    assert_eq!(
        roots.agent_root,
        fx.base_root.join("profiles").join("work").join("agent")
    );
    assert_ne!(roots.agent_root, xdg.join("agent"));
}

#[test]
fn resolve_returns_none_without_home_and_config_env() {
    let inputs = ResolutionInputs::default();
    assert!(omp::resolve(&inputs).is_none());
}

#[test]
fn empty_profile_flag_falls_through_to_env() {
    let fx = Fixture::new();
    let inputs = ResolutionInputs {
        profile_flag: Some(OsString::from("   ")),
        omp_profile_env: Some(OsString::from("env")),
        ..fx.inputs_default()
    };
    let roots = omp::resolve(&inputs).unwrap();
    assert_eq!(
        roots.profile,
        ProfileSelection::Named(OsString::from("env"))
    );
}
