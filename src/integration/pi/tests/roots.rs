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
fn resolves_default_grouped_root_from_home() {
    let fx = Fixture::new();
    let inputs = fx.inputs_default();
    let roots = pi::resolve(&inputs).expect("home present");
    assert!(!roots.custom_session_root);
    assert_eq!(roots.agent_root, fx.agent_root);
    assert_eq!(roots.session_root, fx.session_root);
}

#[test]
fn agent_dir_env_overrides_default_root() {
    let tmp = tempfile::tempdir().unwrap();
    let custom = tmp.path().join("custom-agent");
    fs::create_dir_all(&custom).unwrap();
    let inputs = ResolutionInputs {
        home: Some(tmp.path().to_path_buf()),
        agent_dir_env: Some(custom.clone()),
        session_dir_env: None,
        session_dir_flag: None,
        settings: None,
    };
    let roots = pi::resolve(&inputs).unwrap();
    assert_eq!(roots.agent_root, custom);
    assert_eq!(roots.session_root, custom.join("sessions"));
}

#[test]
fn session_dir_env_overrides_session_root_and_is_flat() {
    let fx = Fixture::new();
    let custom_sessions = fx.agent_root.join("alt-sessions");
    fs::create_dir_all(&custom_sessions).unwrap();
    let inputs = ResolutionInputs {
        home: Some(fx.home()),
        agent_dir_env: None,
        session_dir_env: Some(custom_sessions.clone()),
        session_dir_flag: None,
        settings: None,
    };
    let roots = pi::resolve(&inputs).unwrap();
    assert!(roots.custom_session_root);
    assert_eq!(roots.session_root, custom_sessions);
}

#[test]
fn session_dir_flag_beats_env_and_settings() {
    let fx = Fixture::new();
    let flag_dir = fx.agent_root.join("flag-sessions");
    let env_dir = fx.agent_root.join("env-sessions");
    fs::create_dir_all(&flag_dir).unwrap();
    let settings = json!({ "sessionDir": fx.agent_root.join("settings-sessions") });
    let inputs = ResolutionInputs {
        home: Some(fx.home()),
        agent_dir_env: None,
        session_dir_env: Some(env_dir),
        session_dir_flag: Some(flag_dir.clone()),
        settings: Some(settings),
    };
    let roots = pi::resolve(&inputs).unwrap();
    assert_eq!(roots.session_root, flag_dir);
}

#[test]
fn settings_session_dir_overrides_default() {
    let fx = Fixture::new();
    let settings_dir = fx.agent_root.join("settings-sessions");
    let settings = json!({ "sessionDir": settings_dir.clone() });
    let inputs = ResolutionInputs {
        home: Some(fx.home()),
        agent_dir_env: None,
        session_dir_env: None,
        session_dir_flag: None,
        settings: Some(settings),
    };
    let roots = pi::resolve(&inputs).unwrap();
    assert!(roots.custom_session_root);
    assert_eq!(roots.session_root, settings_dir);
}

#[test]
fn settings_session_dir_as_object_path() {
    let fx = Fixture::new();
    let settings_dir = fx.agent_root.join("obj-sessions");
    let settings = json!({ "sessionDir": { "path": settings_dir.clone() } });
    let inputs = ResolutionInputs {
        home: Some(fx.home()),
        agent_dir_env: None,
        session_dir_env: None,
        session_dir_flag: None,
        settings: Some(settings),
    };
    let roots = pi::resolve(&inputs).unwrap();
    assert_eq!(roots.session_root, settings_dir);
}

#[test]
fn resolve_returns_none_without_home_and_agent_env() {
    let inputs = ResolutionInputs {
        home: None,
        agent_dir_env: None,
        session_dir_env: None,
        session_dir_flag: None,
        settings: None,
    };
    assert!(pi::resolve(&inputs).is_none());
}

// ---------------------------------------------------------------------------
// Header parsing across v1/v2/v3
// ---------------------------------------------------------------------------

#[test]
fn read_settings_returns_none_when_absent() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(pi::read_settings(tmp.path()).is_none());
}

#[test]
fn read_settings_parses_session_dir() {
    let fx = Fixture::new();
    let settings = json!({ "sessionDir": fx.agent_root.join("x") });
    fs::write(
        fx.agent_root.join("settings.json"),
        serde_json::to_string(&settings).unwrap(),
    )
    .unwrap();
    let parsed = pi::read_settings(&fx.agent_root).unwrap();
    assert_eq!(pi::settings_dir_pub(&parsed), Some(fx.agent_root.join("x")));
}

#[test]
fn read_settings_ignores_invalid_json() {
    let fx = Fixture::new();
    fs::write(fx.agent_root.join("settings.json"), "{ not valid").unwrap();
    assert!(pi::read_settings(&fx.agent_root).is_none());
}
