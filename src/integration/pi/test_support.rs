#![allow(unused_imports)]
//! Pi integration tests.
//!
//! Implements the complete Pi fixture matrix from the plan's Tests section:
//! - versions 1, 2, and 3;
//! - named/cleared sessions;
//! - strings, text plus image, and image-only input;
//! - branched parents;
//! - alternate and flat roots;
//! - duplicate IDs across roots;
//! - timestamp fallback;
//! - malformed middle/tail records;
//! - missing header;
//! - missing Workspace;
//! - growing file.
//!
//! Uses a fake `pi` executable that captures exact cwd/argv/env, and asserts
//! discovery/Preview never migrates v1/v2 files or changes any byte/mtime via
//! the shared snapshot helpers.

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
    integration::pi::{
        self, DiscoverConfig, EffectiveRoots, ParsedSession, ResolutionInputs,
        SessionControlEvidence,
    },
    preview::snapshot,
    scope::{Direction, Scope},
};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Tempdir-based fixture builder for Pi sessions.
pub(crate) struct Fixture {
    _tmp: tempfile::TempDir,
    /// agent root: `<tmp>/.pi/agent` (matches `~/.pi/agent` default).
    pub(crate) agent_root: PathBuf,
    /// default session root: `<tmp>/.pi/agent/sessions`
    pub(crate) session_root: PathBuf,
    /// A workspace dir to use as header cwd.
    pub(crate) workspace: PathBuf,
}

impl Fixture {
    pub(crate) fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let agent_root = tmp.path().join(".pi/agent");
        let session_root = agent_root.join("sessions");
        let workspace = tmp.path().join("workspace");
        fs::create_dir_all(&session_root).unwrap();
        fs::create_dir_all(&workspace).unwrap();
        Self {
            _tmp: tmp,
            agent_root,
            session_root,
            workspace,
        }
    }

    pub(crate) fn home(&self) -> PathBuf {
        // The fake home is `<tmp>` so that `$HOME/.pi/agent` == agent_root.
        // agent_root = <tmp>/.pi/agent → parent = <tmp>/.pi → parent = <tmp>.
        self.agent_root
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf()
    }

    /// The grouped directory name Pi would use for `self.workspace`:
    /// `-{absolute path with '/' -> '-'}-`.
    pub(crate) fn encoded_ws(&self) -> String {
        format!("-{}-", self.workspace.display().to_string().replace('/', "-"))
    }

    /// Write a JSONL file into a grouped Workspace dir, returning its path.
    pub(crate) fn write_grouped(&self, encoded_ws: &str, name: &str, records: &[Value]) -> PathBuf {
        let dir = self.session_root.join(encoded_ws);
        fs::create_dir_all(&dir).unwrap();
        self.write_jsonl(&dir.join(name), records)
    }

    pub(crate) fn write_jsonl(&self, path: &Path, records: &[Value]) -> PathBuf {
        let mut file = fs::File::create(path).unwrap();
        for record in records {
            writeln!(file, "{}", serde_json::to_string(record).unwrap()).unwrap();
        }
        file.sync_all().unwrap();
        path.to_path_buf()
    }

    /// Append a raw (possibly partial) record fragment to a file.
    pub(crate) fn append_raw(&self, path: &Path, fragment: &[u8]) {
        let mut file = fs::OpenOptions::new().append(true).open(path).unwrap();
        file.write_all(fragment).unwrap();
        file.sync_all().unwrap();
    }

    pub(crate) fn roots_default(&self) -> EffectiveRoots {
        EffectiveRoots {
            agent_root: self.agent_root.clone(),
            session_root: self.session_root.clone(),
            custom_session_root: false,
        }
    }

    pub(crate) fn roots_custom(&self, custom_root: PathBuf) -> EffectiveRoots {
        EffectiveRoots {
            agent_root: self.agent_root.clone(),
            session_root: custom_root,
            custom_session_root: true,
        }
    }

    pub(crate) fn scope_exact_workspace(&self) -> Scope {
        Scope::new(
            self.workspace.canonicalize().unwrap(),
            None,
            crate::scope::DefaultScope::Exact { git_warning: None },
        )
    }

    /// Discover using the default roots and a workspace-exact scope.
    pub(crate) fn discover_default(&self) -> crate::integration::pi::DiscoverOutcome {
        let roots = self.roots_default();
        let scope = self.scope_exact_workspace();
        let cfg = DiscoverConfig::new(roots, &scope);
        pi::discover(&cfg).unwrap()
    }

    /// Discover using custom roots and a workspace-exact scope.
    pub(crate) fn discover_custom(
        &self,
        roots: EffectiveRoots,
    ) -> crate::integration::pi::DiscoverOutcome {
        let scope = self.scope_exact_workspace();
        let cfg = DiscoverConfig::new(roots, &scope);
        pi::discover(&cfg).unwrap()
    }

    /// Inputs that resolve to this fixture's default roots via $HOME.
    pub(crate) fn inputs_default(&self) -> ResolutionInputs {
        ResolutionInputs {
            home: Some(self.home()),
            agent_dir_env: None,
            session_dir_env: None,
            session_dir_flag: None,
            settings: None,
        }
    }
}

/// Build a v3 session header record.
pub(crate) fn header_v3(id: &str, cwd: &Path, timestamp: u64) -> Value {
    json!({
        "type": "session",
        "v": 3,
        "id": id,
        "cwd": cwd,
        "timestamp": timestamp,
    })
}

/// Build a v2 session header (no `v` field, otherwise same shape).
pub(crate) fn header_v2(id: &str, cwd: &Path, timestamp: u64) -> Value {
    json!({
        "type": "session",
        "id": id,
        "cwd": cwd,
        "timestamp": timestamp,
    })
}

/// Build a v1 session header (older field name `sessionId`, `dir`).
pub(crate) fn header_v1(session_id: &str, cwd: &Path, timestamp: u64) -> Value {
    json!({
        "type": "session",
        "id": session_id,
        "cwd": cwd,
        "timestamp": timestamp,
    })
}

/// Build a user message record with string content.
pub(crate) fn user_message_string(text: &str, timestamp: u64) -> Value {
    json!({
        "type": "user",
        "timestamp": timestamp,
        "message": {
            "role": "user",
            "content": text,
        }
    })
}

/// Build a user message record with typed block content (text + image).
pub(crate) fn user_message_blocks(text: &str, image_media_type: &str, timestamp: u64) -> Value {
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

/// Build an image-only user message (no text).
pub(crate) fn user_message_image_only(media_type: &str, timestamp: u64) -> Value {
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

/// Build a session_info record with a name.
pub(crate) fn session_info(name: &str) -> Value {
    json!({
        "type": "session_info",
        "name": name,
    })
}

/// Build an assistant record (must be excluded from user messages).
pub(crate) fn assistant_message(text: &str) -> Value {
    json!({
        "type": "assistant",
        "message": {
            "role": "assistant",
            "content": text,
        }
    })
}

// ---------------------------------------------------------------------------
// Root resolution tests
// ---------------------------------------------------------------------------
