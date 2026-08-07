use std::{cmp::Ordering, ffi::OsString, path::PathBuf, time::SystemTime};

/// Integration-owned opaque identity. None of these fields is presentation text.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SessionKey {
    pub agent: OsString,
    pub effective_root: PathBuf,
    pub profile: Option<OsString>,
    pub native_locator: OsString,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WorkspaceEvidence {
    Recorded {
        workspace: PathBuf,
        historical_git_identity: Option<OsString>,
    },
    Inferred {
        workspace: PathBuf,
        historical_git_identity: Option<OsString>,
    },
    Unknown,
}

impl WorkspaceEvidence {
    pub fn workspace(&self) -> Option<&std::path::Path> {
        match self {
            Self::Recorded { workspace, .. } | Self::Inferred { workspace, .. } => Some(workspace),
            Self::Unknown => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SupportStatus {
    Supported,
    DiscoverOnly,
    Unsupported,
    Unavailable,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ActivityStatus {
    Active { observed_at: SystemTime },
    Inactive { observed_at: SystemTime },
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RiskStatus {
    Normal,
    BroadWorkspace,
    WorkspaceChanged,
    ConflictingMetadata,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Session {
    pub key: SessionKey,
    /// The integration-owned resumable ID, deliberately distinct from `key`.
    pub resumable_id: OsString,
    pub title: Option<String>,
    pub workspace: WorkspaceEvidence,
    pub support: SupportStatus,
    pub activity: ActivityStatus,
    pub risk: RiskStatus,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResumeSpec {
    pub program: OsString,
    pub argv: Vec<OsString>,
    pub cwd: PathBuf,
    pub env: Vec<(OsString, OsString)>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Diagnostic {
    pub category: &'static str,
    pub count: usize,
    pub verbose_path: Option<PathBuf>,
    pub verbose_chain: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum IntegrationError {
    #[error("integration unavailable")]
    Unavailable,
    #[error("invalid session data")]
    InvalidSession { diagnostic: Diagnostic },
    #[error("I/O failure")]
    Io {
        diagnostic: Diagnostic,
        #[source]
        source: std::io::Error,
    },
}

pub fn compare_sessions(left: &Session, right: &Session) -> Ordering {
    activity_rank(&right.activity)
        .cmp(&activity_rank(&left.activity))
        .then_with(|| activity_time(&right.activity).cmp(&activity_time(&left.activity)))
        .then_with(|| left.key.cmp(&right.key))
}

fn activity_rank(activity: &ActivityStatus) -> u8 {
    match activity {
        ActivityStatus::Active { .. } => 2,
        ActivityStatus::Inactive { .. } => 1,
        ActivityStatus::Unknown => 0,
    }
}

fn activity_time(activity: &ActivityStatus) -> Option<SystemTime> {
    match activity {
        ActivityStatus::Active { observed_at } | ActivityStatus::Inactive { observed_at } => {
            Some(*observed_at)
        }
        ActivityStatus::Unknown => None,
    }
}

pub fn sort_sessions(sessions: &mut [Session]) {
    sessions.sort_by(compare_sessions);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn key(agent: &str, root: &str, profile: Option<&str>, locator: &str) -> SessionKey {
        SessionKey {
            agent: agent.into(),
            effective_root: root.into(),
            profile: profile.map(Into::into),
            native_locator: locator.into(),
        }
    }

    #[test]
    fn every_identity_provenance_dimension_prevents_collision() {
        let keys = [
            key("pi", "/a", None, "id"),
            key("omp", "/a", None, "id"),
            key("pi", "/b", None, "id"),
            key("pi", "/a", Some("work"), "id"),
            key("pi", "/a", None, "other"),
        ];
        assert_eq!(keys.into_iter().collect::<HashSet<_>>().len(), 5);
    }

    #[test]
    fn final_order_is_activity_descending_unknown_last_then_key() {
        fn session(key_name: &str, activity: ActivityStatus) -> Session {
            Session {
                key: key("pi", "/root", None, key_name),
                resumable_id: key_name.into(),
                title: None,
                workspace: WorkspaceEvidence::Unknown,
                support: SupportStatus::Supported,
                activity,
                risk: RiskStatus::Normal,
            }
        }
        let old = SystemTime::UNIX_EPOCH;
        let new = old + std::time::Duration::from_secs(1);
        let mut sessions = vec![
            session("z", ActivityStatus::Unknown),
            session("b", ActivityStatus::Active { observed_at: old }),
            session("a", ActivityStatus::Active { observed_at: new }),
            session("a", ActivityStatus::Inactive { observed_at: new }),
        ];
        sort_sessions(&mut sessions);
        assert_eq!(
            sessions
                .iter()
                .map(|session| session.resumable_id.clone())
                .collect::<Vec<_>>(),
            ["a", "b", "a", "z"].map(OsString::from)
        );
    }

    #[cfg(unix)]
    #[test]
    fn resume_spec_preserves_non_utf8_path_and_argv() {
        use std::os::unix::ffi::{OsStrExt, OsStringExt};
        let bytes = vec![b'x', 0xff];
        let spec = ResumeSpec {
            program: OsString::from_vec(bytes.clone()),
            argv: vec![OsString::from_vec(bytes.clone())],
            cwd: PathBuf::from(OsString::from_vec(bytes.clone())),
            env: vec![],
        };
        assert_eq!(spec.program.as_os_str().as_bytes(), bytes);
        assert_eq!(spec.argv[0].as_os_str().as_bytes(), bytes);
        assert_eq!(spec.cwd.as_os_str().as_bytes(), bytes);
    }
}
