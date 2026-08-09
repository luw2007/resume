use super::{AGENT, discover::ParsedSession, roots::EffectiveRoots};
use crate::session::{
    ActivityStatus, RiskStatus, Session, SessionKey, SupportStatus, WorkspaceEvidence,
};
use std::ffi::OsString;
impl ParsedSession {
    /// Resolve the title: latest `session_info.name`, else summary from the
    /// first valid human message.
    pub fn title(&self) -> Option<String> {
        if let Some(name) = &self.session_info_name {
            let trimmed = name.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
        let texts: Vec<&str> = self.messages.iter().map(|m| m.text.as_str()).collect();
        crate::summary::summarize_texts(texts, crate::summary::default_width())
    }

    /// Build a [`Session`] from this parsed data.
    pub fn into_session(
        self,
        roots: &EffectiveRoots,
        risk: RiskStatus,
        activity: ActivityStatus,
    ) -> Session {
        let workspace_evidence = match &self.workspace {
            Some(workspace) => WorkspaceEvidence::Recorded {
                workspace: workspace.clone(),
                historical_git_identity: None,
            },
            None => WorkspaceEvidence::Unknown,
        };
        let title = self.title();
        Session {
            key: SessionKey {
                agent: OsString::from(AGENT),
                effective_root: roots.session_root.clone(),
                profile: None,
                native_locator: self.transcript_path.clone().into_os_string(),
            },
            resumable_id: OsString::from(self.id),
            title,
            workspace: workspace_evidence,
            support: SupportStatus::Supported,
            activity,
            risk,
        }
    }
}

#[cfg(test)]
#[path = "tests/format.rs"]
mod tests;
