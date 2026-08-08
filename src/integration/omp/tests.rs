//! OMP integration tests.
//!
//! Implements the complete OMP fixture matrix from the plan's Tests section:
//! - default and named profiles;
//! - base/root/profile environment interactions (PI_CONFIG_DIR,
//!   PI_CODING_AGENT_DIR, OMP_PROFILE, PI_PROFILE, XDG_DATA_HOME);
//! - custom session root;
//! - optional existing XDG split roots;
//! - title record plus v3 header (title-before-header);
//! - generated/named filenames;
//! - title changes;
//! - attributed injections;
//! - text/image input;
//! - foreign import metadata;
//! - duplicate IDs across profiles;
//! - live/stale breadcrumbs;
//! - missing Workspace;
//! - empty/malformed/truncated JSONL.
//!
//! Uses a fake `omp` executable that captures exact cwd/argv/env, and asserts
//! discovery/Preview never modifies any byte/mtime via the shared snapshot
//! helpers.

#![cfg(test)]

use std::{
    ffi::OsString,
    fs,
    io::Write,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use serde_json::{Value, json};

use crate::{
    integration::omp::{
        self, ActivityEvidence, DiscoverConfig, EffectiveRoots, ImportBadge, ParsedSession,
        ProfileSelection, ResolutionInputs,
    },
    scope::{Direction, Scope},
    snapshot,
};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Tempdir-based fixture builder for OMP sessions.
struct Fixture {
    _tmp: tempfile::TempDir,
    /// base config root: `<tmp>/.omp`.
    base_root: PathBuf,
    /// default agent root: `<tmp>/.omp/agent`.
    default_agent_root: PathBuf,
    /// A workspace dir to use as header cwd.
    workspace: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let base_root = tmp.path().join(".omp");
        let default_agent_root = base_root.join("agent");
        let workspace = tmp.path().join("workspace");
        fs::create_dir_all(&default_agent_root).unwrap();
        fs::create_dir_all(&workspace).unwrap();
        Self {
            _tmp: tmp,
            base_root,
            default_agent_root,
            workspace,
        }
    }

    fn home(&self) -> PathBuf {
        // base_root = <tmp>/.omp → parent = <tmp>.
        self.base_root.parent().unwrap().to_path_buf()
    }

    /// Named profile agent root: `<base>/profiles/<name>/agent`.
    fn profile_agent_root(&self, name: &str) -> PathBuf {
        let root = self.base_root.join("profiles").join(name).join("agent");
        fs::create_dir_all(&root).unwrap();
        root
    }

    /// Write a JSONL file into a subdirectory of a session root.
    fn write(&self, session_root: &Path, subdir: &str, name: &str, records: &[Value]) -> PathBuf {
        let dir = session_root.join(subdir);
        fs::create_dir_all(&dir).unwrap();
        self.write_jsonl(&dir.join(name), records)
    }

    /// Write a JSONL file directly under a session root.
    fn write_flat(&self, session_root: &Path, name: &str, records: &[Value]) -> PathBuf {
        fs::create_dir_all(session_root).unwrap();
        self.write_jsonl(&session_root.join(name), records)
    }

    fn write_jsonl(&self, path: &Path, records: &[Value]) -> PathBuf {
        let mut file = fs::File::create(path).unwrap();
        for record in records {
            writeln!(file, "{}", serde_json::to_string(record).unwrap()).unwrap();
        }
        file.sync_all().unwrap();
        path.to_path_buf()
    }

    fn append_raw(&self, path: &Path, fragment: &[u8]) {
        let mut file = fs::OpenOptions::new().append(true).open(path).unwrap();
        file.write_all(fragment).unwrap();
        file.sync_all().unwrap();
    }

    fn roots_default(&self) -> EffectiveRoots {
        EffectiveRoots {
            config_root: self.base_root.clone(),
            agent_root: self.default_agent_root.clone(),
            session_root: self.default_agent_root.clone(),
            custom_session_root: false,
            profile: ProfileSelection::Default,
        }
    }

    fn roots_named(&self, name: &str) -> EffectiveRoots {
        let agent = self.profile_agent_root(name);
        EffectiveRoots {
            config_root: self.base_root.clone(),
            agent_root: agent.clone(),
            session_root: agent,
            custom_session_root: false,
            profile: ProfileSelection::Named(OsString::from(name)),
        }
    }

    fn roots_custom(&self, session_root: PathBuf, profile: ProfileSelection) -> EffectiveRoots {
        let agent_root = match &profile {
            ProfileSelection::Default => self.default_agent_root.clone(),
            ProfileSelection::Named(name) => {
                self.base_root.join("profiles").join(name).join("agent")
            }
        };
        EffectiveRoots {
            config_root: self.base_root.clone(),
            agent_root,
            session_root,
            custom_session_root: true,
            profile,
        }
    }

    fn scope_exact_workspace(&self) -> Scope {
        Scope::new(
            self.workspace.canonicalize().unwrap(),
            None,
            crate::scope::DefaultScope::Exact { git_warning: None },
        )
    }

    fn discover(&self, roots: EffectiveRoots) -> omp::DiscoverOutcome {
        let scope = self.scope_exact_workspace();
        let cfg = DiscoverConfig::new(roots, &scope);
        omp::discover(&cfg).unwrap()
    }

    fn inputs_default(&self) -> ResolutionInputs {
        ResolutionInputs {
            home: Some(self.home()),
            config_dir_env: None,
            agent_dir_env: None,
            session_dir_flag: None,
            xdg_data_home: None,
            profile_flag: None,
            omp_profile_env: None,
            pi_profile_env: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Record builders
// ---------------------------------------------------------------------------

/// Padded v1 title sidecar that PRECEDES the v3 session header.
fn title_sidecar(title: &str) -> Value {
    json!({
        "type": "title",
        "v": 1,
        "title": title,
    })
}

/// v3 session header.
fn header_v3(id: &str, cwd: &Path, timestamp: u64) -> Value {
    json!({
        "type": "session",
        "v": 3,
        "id": id,
        "cwd": cwd,
        "timestamp": timestamp,
    })
}

/// v3 session header with title metadata.
fn header_v3_titled(id: &str, cwd: &Path, timestamp: u64, title: &str) -> Value {
    json!({
        "type": "session",
        "v": 3,
        "id": id,
        "cwd": cwd,
        "timestamp": timestamp,
        "title": title,
    })
}

fn title_change(title: &str) -> Value {
    json!({
        "type": "title_change",
        "title": title,
    })
}

fn user_message_string(text: &str, timestamp: u64) -> Value {
    json!({
        "type": "user",
        "timestamp": timestamp,
        "message": {
            "role": "user",
            "content": text,
        }
    })
}

fn user_message_blocks(text: &str, image_media_type: &str, timestamp: u64) -> Value {
    json!({
        "type": "user",
        "timestamp": timestamp,
        "message": {
            "role": "user",
            "content": [
                { "type": "text", "text": text },
                { "type": "image", "media_type": image_media_type, "data": "iVBORw0KGgo=" }
            ]
        }
    })
}

fn user_message_image_only(media_type: &str, timestamp: u64) -> Value {
    json!({
        "type": "user",
        "timestamp": timestamp,
        "message": {
            "role": "user",
            "content": [
                { "type": "image", "media_type": media_type, "data": "iVBORw0KGgo=" }
            ]
        }
    })
}

/// An agent-injected user-role message (must be filtered out by attribution).
fn injected_user_message(text: &str, timestamp: u64) -> Value {
    json!({
        "type": "user",
        "timestamp": timestamp,
        "attribution": { "source": "injected" },
        "message": {
            "role": "user",
            "content": text,
        }
    })
}

fn assistant_message(text: &str) -> Value {
    json!({
        "type": "assistant",
        "message": { "role": "assistant", "content": text },
    })
}

fn foreign_import(source_kind: &str, origin_id: &str, origin_cwd: &Path) -> Value {
    json!({
        "type": "custom",
        "foreign_session_import": {
            "source_kind": source_kind,
            "origin_id": origin_id,
            "origin_cwd": origin_cwd,
        }
    })
}

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

// ===========================================================================
// HEADER PARSING: title-before-header (do NOT reuse Pi assumptions)
// ===========================================================================

#[test]
fn parses_v3_header_when_title_sidecar_precedes_it() {
    let fx = Fixture::new();
    fx.write(
        &fx.default_agent_root,
        "ws",
        "s.jsonl",
        &[
            title_sidecar("Initial Title"),
            header_v3("abc-123", &fx.workspace, 1700000000),
            user_message_string("hello world", 1700000010),
        ],
    );
    let outcome = fx.discover(fx.roots_default());
    assert_eq!(outcome.parsed.len(), 1);
    let parsed = &outcome.parsed[0];
    assert_eq!(parsed.id, "abc-123");
    assert_eq!(parsed.workspace.as_ref().unwrap(), &fx.workspace);
    assert_eq!(parsed.messages.len(), 1);
    assert_eq!(parsed.messages[0].text, "hello world");
}

#[test]
fn file_without_session_header_skipped_as_no_header() {
    let fx = Fixture::new();
    // Title sidecar present but no session header.
    fx.write(
        &fx.default_agent_root,
        "ws",
        "nohdr.jsonl",
        &[
            title_sidecar("Only Title"),
            user_message_string("hello", 1700000000),
        ],
    );
    let outcome = fx.discover(fx.roots_default());
    assert_eq!(outcome.parsed.len(), 0);
    assert_eq!(outcome.no_header_files, 1);
}

#[test]
fn header_not_required_to_be_first_record() {
    // A title record, then an unknown record, then the header, then a user
    // message. The header must still be found.
    let fx = Fixture::new();
    let unknown = json!({ "type": "unknown_future_record", "foo": "bar" });
    fx.write(
        &fx.default_agent_root,
        "ws",
        "order.jsonl",
        &[
            title_sidecar("T"),
            unknown,
            header_v3("late-hdr", &fx.workspace, 1700000000),
            user_message_string("after", 1700000010),
        ],
    );
    let outcome = fx.discover(fx.roots_default());
    assert_eq!(outcome.parsed.len(), 1);
    assert_eq!(outcome.parsed[0].id, "late-hdr");
}

// ===========================================================================
// TITLE RESOLUTION: header / title / title_change
// ===========================================================================

#[test]
fn title_sidecar_provides_initial_title() {
    let fx = Fixture::new();
    fx.write(
        &fx.default_agent_root,
        "ws",
        "t.jsonl",
        &[
            title_sidecar("Sidecar Title"),
            header_v3("t", &fx.workspace, 1700000000),
            user_message_string("msg", 1700000010),
        ],
    );
    let outcome = fx.discover(fx.roots_default());
    assert_eq!(outcome.parsed[0].title.as_deref(), Some("Sidecar Title"));
}

#[test]
fn header_title_metadata_wins_over_earlier_sidecar() {
    let fx = Fixture::new();
    fx.write(
        &fx.default_agent_root,
        "ws",
        "h.jsonl",
        &[
            title_sidecar("Old"),
            header_v3_titled("h", &fx.workspace, 1700000000, "Header Title"),
        ],
    );
    let outcome = fx.discover(fx.roots_default());
    assert_eq!(outcome.parsed[0].title.as_deref(), Some("Header Title"));
}

#[test]
fn title_change_overrides_header_and_sidecar() {
    let fx = Fixture::new();
    fx.write(
        &fx.default_agent_root,
        "ws",
        "tc.jsonl",
        &[
            title_sidecar("Side"),
            header_v3_titled("tc", &fx.workspace, 1700000000, "Header"),
            title_change("Changed"),
        ],
    );
    let outcome = fx.discover(fx.roots_default());
    assert_eq!(outcome.parsed[0].title.as_deref(), Some("Changed"));
}

#[test]
fn latest_title_change_wins() {
    let fx = Fixture::new();
    fx.write(
        &fx.default_agent_root,
        "ws",
        "multi.jsonl",
        &[
            header_v3("multi", &fx.workspace, 1700000000),
            title_change("First"),
            title_change("Second"),
            title_change("Third"),
        ],
    );
    let outcome = fx.discover(fx.roots_default());
    assert_eq!(outcome.parsed[0].title.as_deref(), Some("Third"));
}

#[test]
fn title_falls_back_to_summary_from_first_human_input() {
    let fx = Fixture::new();
    fx.write(
        &fx.default_agent_root,
        "ws",
        "fallback.jsonl",
        &[
            header_v3("fb", &fx.workspace, 1700000000),
            user_message_string("Fix the parser bug now", 1700000010),
            user_message_string("second", 1700000020),
        ],
    );
    let outcome = fx.discover(fx.roots_default());
    let title = outcome.parsed[0].title.clone().unwrap();
    assert!(title.starts_with("Fix the parser bug now"));
}

#[test]
fn title_none_when_no_title_and_no_user_messages() {
    let fx = Fixture::new();
    fx.write(
        &fx.default_agent_root,
        "ws",
        "empty.jsonl",
        &[header_v3("empty", &fx.workspace, 1700000000)],
    );
    let outcome = fx.discover(fx.roots_default());
    assert!(outcome.parsed[0].title.is_none());
}

// ===========================================================================
// USER MESSAGE EXTRACTION + attribution filtering
// ===========================================================================

#[test]
fn extracts_string_user_message() {
    let fx = Fixture::new();
    fx.write(
        &fx.default_agent_root,
        "ws",
        "s.jsonl",
        &[
            header_v3("s", &fx.workspace, 1700000000),
            user_message_string("hello", 1700000010),
        ],
    );
    let outcome = fx.discover(fx.roots_default());
    assert_eq!(outcome.parsed[0].messages.len(), 1);
    assert_eq!(outcome.parsed[0].messages[0].text, "hello");
}

#[test]
fn extracts_text_plus_image_with_placeholder_not_base64() {
    let fx = Fixture::new();
    fx.write(
        &fx.default_agent_root,
        "ws",
        "img.jsonl",
        &[
            header_v3("img", &fx.workspace, 1700000000),
            user_message_blocks("look here", "image/png", 1700000010),
        ],
    );
    let outcome = fx.discover(fx.roots_default());
    let msg = &outcome.parsed[0].messages[0];
    assert_eq!(msg.text, "look here");
    assert_eq!(msg.attachments.len(), 1);
    let display = msg.attachments[0].to_display();
    assert!(display.contains("[image]"));
    assert!(display.contains("image/png"));
    assert!(!display.contains("iVBOR"));
}

#[test]
fn extracts_image_only_message() {
    let fx = Fixture::new();
    fx.write(
        &fx.default_agent_root,
        "ws",
        "imgonly.jsonl",
        &[
            header_v3("io", &fx.workspace, 1700000000),
            user_message_image_only("image/jpeg", 1700000010),
        ],
    );
    let outcome = fx.discover(fx.roots_default());
    let msg = &outcome.parsed[0].messages[0];
    assert!(msg.text.is_empty());
    assert_eq!(msg.attachments.len(), 1);
}

#[test]
fn excludes_assistant_messages() {
    let fx = Fixture::new();
    fx.write(
        &fx.default_agent_root,
        "ws",
        "a.jsonl",
        &[
            header_v3("a", &fx.workspace, 1700000000),
            user_message_string("user q", 1700000010),
            assistant_message("agent a"),
        ],
    );
    let outcome = fx.discover(fx.roots_default());
    assert_eq!(outcome.parsed[0].messages.len(), 1);
    assert_eq!(outcome.parsed[0].messages[0].text, "user q");
}

#[test]
fn excludes_injected_user_messages_by_attribution() {
    let fx = Fixture::new();
    fx.write(
        &fx.default_agent_root,
        "ws",
        "inj.jsonl",
        &[
            header_v3("inj", &fx.workspace, 1700000000),
            injected_user_message("agent-injected text", 1700000010),
            user_message_string("real user text", 1700000020),
        ],
    );
    let outcome = fx.discover(fx.roots_default());
    let msgs = &outcome.parsed[0].messages;
    assert_eq!(msgs.len(), 1, "injected user-role message must be excluded");
    assert_eq!(msgs[0].text, "real user text");
}

#[test]
fn injection_wrappers_collapsed_in_user_messages() {
    let fx = Fixture::new();
    fx.write(
        &fx.default_agent_root,
        "ws",
        "wrap.jsonl",
        &[
            header_v3("wrap", &fx.workspace, 1700000000),
            user_message_string("<skill>hidden</skill> visible", 1700000010),
        ],
    );
    let outcome = fx.discover(fx.roots_default());
    assert_eq!(outcome.parsed[0].messages[0].text, "hidden visible");
}

// ===========================================================================
// FOREIGN SESSION IMPORT → safe badge, new OMP ID retained
// ===========================================================================

#[test]
fn import_creates_safe_badge_and_keeps_new_omp_id() {
    let fx = Fixture::new();
    let origin_cwd = fx.home().join("origin-repo");
    fs::create_dir_all(&origin_cwd).unwrap();
    fx.write(
        &fx.default_agent_root,
        "ws",
        "imp.jsonl",
        &[
            title_sidecar("Imported Session"),
            header_v3("omp-new-id", &fx.workspace, 1700000000),
            foreign_import("codex", "codex-origin-id-1234", &origin_cwd),
        ],
    );
    let outcome = fx.discover(fx.roots_default());
    assert_eq!(outcome.parsed.len(), 1);
    let parsed = &outcome.parsed[0];
    // Resumable identity is the NEW OMP header id, never the origin id.
    assert_eq!(parsed.id, "omp-new-id");
    let badge = parsed.import.as_ref().expect("import badge present");
    assert_eq!(badge.source_kind, "codex");
    assert_eq!(badge.origin_id.as_deref(), Some("codex-origin-id-1234"));
    assert_eq!(badge.origin_cwd.as_deref(), Some(origin_cwd.as_path()));
    // The badge display must not expose the full origin id as resumable.
    let display = badge.to_display();
    assert!(display.contains("imported from codex"));
    assert!(display.contains("origin:codex-or"));
    assert!(!display.contains("codex-origin-id-1234"));
}

#[test]
fn import_never_merges_with_origin_session_identity() {
    // The native_locator is the OMP transcript path; the key never references
    // the origin locator. Even with the same origin_id across two OMP files,
    // they remain distinct OMP sessions.
    let fx = Fixture::new();
    let origin_cwd = fx.home().join("origin");
    fs::create_dir_all(&origin_cwd).unwrap();
    let p1 = fx.write(
        &fx.default_agent_root,
        "ws",
        "a.jsonl",
        &[
            header_v3("omp-1", &fx.workspace, 1700000000),
            foreign_import("claude", "shared-origin-id", &origin_cwd),
        ],
    );
    let p2 = fx.write(
        &fx.default_agent_root,
        "ws",
        "b.jsonl",
        &[
            header_v3("omp-2", &fx.workspace, 1700000000),
            foreign_import("claude", "shared-origin-id", &origin_cwd),
        ],
    );
    let outcome = fx.discover(fx.roots_default());
    assert_eq!(outcome.parsed.len(), 2);
    assert_ne!(outcome.parsed[0].id, outcome.parsed[1].id);
    assert_ne!(
        outcome.parsed[0].transcript_path,
        outcome.parsed[1].transcript_path
    );
    let _ = (p1, p2);
}

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
        "ws",
        "dup.jsonl",
        &[
            header_v3("dup", &fx.workspace, 1700000000),
            user_message_string("x", 1700000010),
        ],
    );
    #[cfg(unix)]
    {
        let link = fx.default_agent_root.join("ws").join("dup-link.jsonl");
        std::os::unix::fs::symlink(&path, &link).unwrap();
    }
    let outcome = fx.discover(fx.roots_default());
    #[cfg(unix)]
    assert_eq!(outcome.parsed.len(), 1);
    #[cfg(not(unix))]
    assert_eq!(outcome.parsed.len(), 1);
}

// ===========================================================================
// SCOPE FILTERING
// ===========================================================================

#[test]
fn out_of_scope_workspace_excluded() {
    let fx = Fixture::new();
    let other_ws = fx.home().join("other-ws");
    fs::create_dir_all(&other_ws).unwrap();
    fx.write(
        &fx.default_agent_root,
        "ws",
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
        "ws",
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
// TIMESTAMP FALLBACK CHAIN
// ===========================================================================

#[test]
fn activity_time_prefers_message_then_header_then_mtime() {
    let fx = Fixture::new();
    let path = fx.write(
        &fx.default_agent_root,
        "ws",
        "ts.jsonl",
        &[
            header_v3("ts", &fx.workspace, 1700000000),
            user_message_string("hi", 1700000050),
        ],
    );
    let outcome = fx.discover(fx.roots_default());
    let parsed = &outcome.parsed[0];
    assert_eq!(
        parsed.activity_time,
        Some(SystemTime::UNIX_EPOCH + Duration::from_secs(1700000050))
    );

    // Header-only → header time.
    let bounds = crate::jsonl::Bounds::default();
    let path2 = fx.write(
        &fx.default_agent_root,
        "ws",
        "ho.jsonl",
        &[header_v3("ho", &fx.workspace, 1700000000)],
    );
    let result2 = crate::jsonl::read_file(&path2, &bounds).unwrap();
    let parsed2 = omp::extract_session_pub(&path2, &result2, None).unwrap();
    assert_eq!(
        parsed2.activity_time,
        Some(SystemTime::UNIX_EPOCH + Duration::from_secs(1700000000))
    );

    // No header timestamp → mtime.
    let header_no_ts = json!({ "type": "session", "id": "nts", "cwd": fx.workspace });
    let path3 = fx.write(&fx.default_agent_root, "ws", "nts.jsonl", &[header_no_ts]);
    let mtime = fs::metadata(&path3).unwrap().modified().unwrap();
    let result3 = crate::jsonl::read_file(&path3, &bounds).unwrap();
    let parsed3 = omp::extract_session_pub(&path3, &result3, Some(mtime)).unwrap();
    assert_eq!(parsed3.activity_time, Some(mtime));
    let _ = path;
}

// ===========================================================================
// MALFORMED / TRUNCATED / EMPTY / MISSING WORKSPACE
// ===========================================================================

#[test]
fn malformed_middle_record_does_not_abort_discovery() {
    let fx = Fixture::new();
    let path = fx.default_agent_root.join("ws").join("mid.jsonl");
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
    let path = fx.default_agent_root.join("ws").join("trunc.jsonl");
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
        "ws",
        "nws.jsonl",
        &[header, user_message_string("hi", 1700000010)],
    );
    let outcome = fx.discover(fx.roots_default());
    assert_eq!(outcome.parsed.len(), 1);
    assert!(outcome.parsed[0].workspace.is_none());
}

// ===========================================================================
// ResumeSpec: default, named profile, custom session-dir, env preservation
// ===========================================================================

#[test]
fn resume_spec_default_is_resume_id() {
    let fx = Fixture::new();
    fx.write(
        &fx.default_agent_root,
        "ws",
        "r.jsonl",
        &[
            header_v3("resume-id", &fx.workspace, 1700000000),
            user_message_string("x", 1700000010),
        ],
    );
    let outcome = fx.discover(fx.roots_default());
    let spec = outcome.parsed[0].resume_spec(&fx.roots_default());

    assert_eq!(spec.program, OsString::from("omp"));
    assert_eq!(
        spec.argv,
        vec![OsString::from("--resume"), OsString::from("resume-id")]
    );
    assert_eq!(spec.cwd, fx.workspace);
}

#[test]
fn resume_spec_named_profile_adds_profile_flag() {
    let fx = Fixture::new();
    let named_root = fx.profile_agent_root("work");
    fx.write_flat(
        &named_root,
        "r.jsonl",
        &[
            header_v3("resume-id", &fx.workspace, 1700000000),
            user_message_string("x", 1700000010),
        ],
    );
    let outcome = fx.discover(fx.roots_named("work"));
    let spec = outcome.parsed[0].resume_spec(&fx.roots_named("work"));

    assert_eq!(
        spec.argv,
        vec![
            OsString::from("--profile"),
            OsString::from("work"),
            OsString::from("--resume"),
            OsString::from("resume-id"),
        ]
    );
    // No --session-dir for default (non-custom) root.
    assert!(!spec.argv.iter().any(|a| a == "--session-dir"));
}

#[test]
fn resume_spec_custom_session_dir_preserved() {
    let fx = Fixture::new();
    let custom = fx.home().join("custom-sessions");
    fs::create_dir_all(&custom).unwrap();
    let roots = fx.roots_custom(custom.clone(), ProfileSelection::Default);
    fx.write_flat(
        &custom,
        "c.jsonl",
        &[
            header_v3("custom-id", &fx.workspace, 1700000000),
            user_message_string("y", 1700000010),
        ],
    );
    let outcome = fx.discover(roots.clone());
    let spec = outcome.parsed[0].resume_spec(&roots);

    let dir_idx = spec.argv.iter().position(|a| a == "--session-dir").unwrap();
    assert_eq!(
        PathBuf::from(spec.argv[dir_idx + 1].clone())
            .canonicalize()
            .unwrap(),
        custom.canonicalize().unwrap()
    );
    // --session-dir comes after --resume <id>.
    let resume_idx = spec.argv.iter().position(|a| a == "--resume").unwrap();
    assert!(dir_idx > resume_idx);
}

#[test]
fn resume_spec_preserves_config_root_env() {
    let fx = Fixture::new();
    fx.write(
        &fx.default_agent_root,
        "ws",
        "e.jsonl",
        &[
            header_v3("e", &fx.workspace, 1700000000),
            user_message_string("x", 1700000010),
        ],
    );
    let outcome = fx.discover(fx.roots_default());
    let spec = outcome.parsed[0].resume_spec(&fx.roots_default());
    let env_map: std::collections::HashMap<OsString, OsString> = spec.env.into_iter().collect();
    assert_eq!(
        env_map
            .get(OsString::from("PI_CONFIG_DIR").as_os_str())
            .map(PathBuf::from),
        Some(fx.base_root.clone())
    );
}

#[test]
fn resume_spec_cwd_falls_back_when_workspace_missing() {
    let fx = Fixture::new();
    let header = json!({ "type": "session", "id": "nws2", "timestamp": 1700000000u64 });
    fx.write(
        &fx.default_agent_root,
        "ws",
        "nws2.jsonl",
        &[header, user_message_string("z", 1700000010)],
    );
    let outcome = fx.discover(fx.roots_default());
    let spec = outcome.parsed[0].resume_spec(&fx.roots_default());
    assert_eq!(spec.cwd, PathBuf::from("."));
}

// ===========================================================================
// ACTIVITY: positive-evidence-only (live process + TTY + breadcrumb)
// ===========================================================================

#[test]
fn activity_unknown_without_evidence() {
    let fx = Fixture::new();
    fx.write(
        &fx.default_agent_root,
        "ws",
        "act.jsonl",
        &[
            header_v3("act", &fx.workspace, 1700000000),
            user_message_string("hi", 1700000010),
        ],
    );
    let outcome = fx.discover(fx.roots_default());
    assert_eq!(
        omp::activity_status(&outcome.parsed[0], None),
        crate::session::ActivityStatus::Unknown
    );
}

#[test]
fn activity_active_only_with_live_process_tty_and_matching_breadcrumb() {
    let fx = Fixture::new();
    let path = fx.write(
        &fx.default_agent_root,
        "ws",
        "act2.jsonl",
        &[
            header_v3("act2", &fx.workspace, 1700000000),
            user_message_string("hi", 1700000010),
        ],
    );
    let outcome = fx.discover(fx.roots_default());
    let parsed = &outcome.parsed[0];
    let now = SystemTime::now();

    // Full positive evidence → Active.
    let evidence = ActivityEvidence {
        live_process: true,
        tty: Some(OsString::from("/dev/ttys001")),
        breadcrumb_alive: true,
        breadcrumb_session_path: path.clone(),
        observed_at: now,
    };
    assert_eq!(
        omp::activity_status(parsed, Some(&evidence)),
        crate::session::ActivityStatus::Active { observed_at: now }
    );

    // No live process → Unknown.
    let no_proc = ActivityEvidence {
        live_process: false,
        ..evidence.clone()
    };
    assert_eq!(
        omp::activity_status(parsed, Some(&no_proc)),
        crate::session::ActivityStatus::Unknown
    );

    // Stale breadcrumb alone → Unknown.
    let stale = ActivityEvidence {
        breadcrumb_alive: false,
        ..evidence.clone()
    };
    assert_eq!(
        omp::activity_status(parsed, Some(&stale)),
        crate::session::ActivityStatus::Unknown
    );

    // No TTY → Unknown.
    let no_tty = ActivityEvidence {
        tty: None,
        ..evidence.clone()
    };
    assert_eq!(
        omp::activity_status(parsed, Some(&no_tty)),
        crate::session::ActivityStatus::Unknown
    );

    // Mismatched breadcrumb path → Unknown.
    let bad_path = ActivityEvidence {
        breadcrumb_session_path: fx.workspace.join("nope.jsonl"),
        ..evidence
    };
    assert_eq!(
        omp::activity_status(parsed, Some(&bad_path)),
        crate::session::ActivityStatus::Unknown
    );
}

// ===========================================================================
// SESSION CONSTRUCTION + RISK
// ===========================================================================

#[test]
fn into_session_builds_supported_session_with_profile_identity() {
    let fx = Fixture::new();
    let named_root = fx.profile_agent_root("work");
    fx.write_flat(
        &named_root,
        "s.jsonl",
        &[
            title_sidecar("Work Session"),
            header_v3("s", &fx.workspace, 1700000000),
        ],
    );
    let outcome = fx.discover(fx.roots_named("work"));
    let session = outcome.parsed[0].clone().into_session(
        &fx.roots_named("work"),
        crate::session::RiskStatus::Normal,
        crate::session::ActivityStatus::Unknown,
    );
    assert_eq!(session.key.agent, OsString::from("omp"));
    assert_eq!(
        session.key.profile.as_deref(),
        Some(std::ffi::OsStr::new("work"))
    );
    assert_eq!(session.resumable_id, OsString::from("s"));
    assert_eq!(session.title.as_deref(), Some("Work Session"));
    assert_eq!(session.support, crate::session::SupportStatus::Supported);
}

#[test]
fn broad_workspace_risk_flagged_for_home_and_root() {
    let parsed = ParsedSession {
        id: "r".into(),
        workspace: Some(PathBuf::from("/")),
        header_time: None,
        title: None,
        messages: vec![],
        transcript_path: PathBuf::from("/x.jsonl"),
        file_mtime: None,
        activity_time: None,
        import: None,
    };
    assert_eq!(
        omp::risk_status(&parsed, Some(Path::new("/"))),
        crate::session::RiskStatus::BroadWorkspace
    );
}

// ===========================================================================
// READ-ONLY INVARIANT: discovery never modifies files
// ===========================================================================

#[test]
fn discovery_does_not_modify_files_bytes_or_mtimes() {
    let fx = Fixture::new();
    let path = fx.write(
        &fx.default_agent_root,
        "ws",
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
        "ws",
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
        "ws",
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

// ===========================================================================
// FAKE `omp` LAUNCH PROVENANCE: exact cwd/argv/env
// ===========================================================================

/// Build a fake `omp` binary that records cwd + argv + PI_CONFIG_DIR to a
/// capture file and exits 0.
#[cfg(unix)]
fn fake_omp(capture_path: &Path) -> PathBuf {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("omp");
    let capture = capture_path.display().to_string();
    let script = format!(
        "#!/bin/sh\n\
         printf '%s\\0' \"$PWD\" >> \"{capture}\"\n\
         for a in \"$@\"; do printf '%s\\0' \"$a\" >> \"{capture}\"; done\n\
         printf 'PI_CONFIG_DIR=%s\\0' \"$PI_CONFIG_DIR\" >> \"{capture}\"\n",
    );
    fs::write(&bin, script).unwrap();
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(&bin).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&bin, perms).unwrap();
    std::mem::forget(dir);
    bin
}

#[cfg(unix)]
fn run_resume_spec_capturing(spec: &crate::session::ResumeSpec) -> std::io::Result<()> {
    use std::process::Command;
    let mut cmd = Command::new(&spec.program);
    cmd.args(&spec.argv);
    cmd.current_dir(&spec.cwd);
    cmd.env_clear();
    cmd.env("HOME", &spec.cwd);
    cmd.env("XDG_CONFIG_HOME", spec.cwd.join(".xdg-config"));
    cmd.env("XDG_DATA_HOME", spec.cwd.join(".xdg-data"));
    cmd.env("XDG_STATE_HOME", spec.cwd.join(".xdg-state"));
    cmd.env("XDG_CACHE_HOME", spec.cwd.join(".xdg-cache"));
    for (k, v) in &spec.env {
        cmd.env(k, v);
    }
    let status = cmd.status()?;
    assert!(status.success(), "fake omp must exit 0");
    Ok(())
}

fn read_capture(capture_path: &Path) -> Vec<String> {
    let data = fs::read(capture_path).unwrap();
    data.split(|b| *b == 0)
        .filter(|f| !f.is_empty())
        .map(|f| String::from_utf8_lossy(f).into_owned())
        .collect()
}

#[cfg(unix)]
#[test]
fn fake_omp_captures_exact_cwd_argv_for_default_profile() {
    let fx = Fixture::new();
    let capture = tempfile::NamedTempFile::new().unwrap();
    let capture_path = capture.path().to_path_buf();
    let fake_bin = fake_omp(&capture_path);

    fx.write(
        &fx.default_agent_root,
        "ws",
        "exec.jsonl",
        &[
            header_v3("exec", &fx.workspace, 1700000000),
            user_message_string("e", 1700000010),
        ],
    );
    let outcome = fx.discover(fx.roots_default());
    let mut spec = outcome.parsed[0].resume_spec(&fx.roots_default());
    spec.program = fake_bin.into_os_string();
    run_resume_spec_capturing(&spec).unwrap();

    let fields = read_capture(&capture_path);
    // fields[0] = cwd, fields[1..] = argv, last = PI_CONFIG_DIR env line.
    assert_eq!(
        PathBuf::from(&fields[0]).canonicalize().unwrap(),
        fx.workspace.canonicalize().unwrap()
    );
    assert_eq!(fields[1], "--resume");
    assert_eq!(fields[2], "exec");
    // PI_CONFIG_DIR preserved.
    assert!(
        fields.iter().any(
            |f| f.starts_with("PI_CONFIG_DIR=") && f.contains(&*fx.base_root.to_string_lossy())
        )
    );
}

#[cfg(unix)]
#[test]
fn fake_omp_captures_profile_and_session_dir() {
    let fx = Fixture::new();
    let custom = fx.home().join("custom-sessions");
    fs::create_dir_all(&custom).unwrap();
    let roots = fx.roots_custom(
        custom.clone(),
        ProfileSelection::Named(OsString::from("work")),
    );
    // The named profile's agent root under the custom-session roots helper.
    fx.write_flat(
        &custom,
        "p.jsonl",
        &[
            header_v3("p", &fx.workspace, 1700000000),
            user_message_string("p", 1700000010),
        ],
    );
    let capture = tempfile::NamedTempFile::new().unwrap();
    let capture_path = capture.path().to_path_buf();
    let fake_bin = fake_omp(&capture_path);

    let outcome = fx.discover(roots.clone());
    let mut spec = outcome.parsed[0].resume_spec(&roots);
    spec.program = fake_bin.into_os_string();
    run_resume_spec_capturing(&spec).unwrap();

    let fields = read_capture(&capture_path);
    // argv = --profile work --resume p --session-dir <custom>
    assert_eq!(fields[1], "--profile");
    assert_eq!(fields[2], "work");
    assert_eq!(fields[3], "--resume");
    assert_eq!(fields[4], "p");
    assert_eq!(fields[5], "--session-dir");
    assert_eq!(
        PathBuf::from(&fields[6]).canonicalize().unwrap(),
        custom.canonicalize().unwrap()
    );
}

// ===========================================================================
// IMPORT BADGE UNIT TESTS
// ===========================================================================

#[test]
fn import_badge_display_truncates_origin_id() {
    let badge = ImportBadge {
        source_kind: "codex".into(),
        origin_id: Some("abcdef1234567890".into()),
        origin_cwd: None,
    };
    let display = badge.to_display();
    assert!(display.contains("imported from codex"));
    assert!(display.contains("origin:abcdef1"));
    assert!(!display.contains("abcdef1234567890"));
}

#[test]
fn import_badge_without_origin_id() {
    let badge = ImportBadge {
        source_kind: "claude".into(),
        origin_id: None,
        origin_cwd: None,
    };
    let display = badge.to_display();
    assert_eq!(display, "imported from claude");
}

#[test]
fn parse_import_pub_handles_alternate_keys() {
    let v = json!({
        "kind": "codex",
        "source_id": "sid",
        "source_cwd": "/path",
    });
    let badge = omp::parse_import_pub(&v).unwrap();
    assert_eq!(badge.source_kind, "codex");
    assert_eq!(badge.origin_id.as_deref(), Some("sid"));
    assert_eq!(
        badge.origin_cwd.as_deref(),
        Some(std::path::Path::new("/path"))
    );
}
