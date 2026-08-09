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
            config_root_overridden: false,
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
            config_root_overridden: false,
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
            config_root_overridden: false,
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

mod activity;
mod discover;
mod format;
mod resume;
mod roots;
