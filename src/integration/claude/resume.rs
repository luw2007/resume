use super::{AGENT, CONFIG_DIR_ENV, roots::ClaudeRoot};
use crate::session::{Diagnostic, IntegrationError, ResumeSpec, Session};
use std::ffi::OsString;
/// Build the exact Resume spec for a Claude Session.
///
/// `claude --resume <uuid>` with the authoritative Workspace as the child cwd.
/// A nondefault `CLAUDE_CONFIG_DIR` is preserved as an environment override.
/// Never `--continue`.
pub fn resume_spec(session: &Session, root: &ClaudeRoot) -> Result<ResumeSpec, IntegrationError> {
    let workspace =
        session
            .workspace
            .workspace()
            .ok_or_else(|| IntegrationError::InvalidSession {
                diagnostic: Diagnostic {
                    category: "claude_missing_workspace",
                    count: 1,
                    verbose_path: None,
                    verbose_chain: Some("no recorded cwd; cannot resume".into()),
                },
            })?;

    let argv = vec![OsString::from("--resume"), session.resumable_id.clone()];

    let env = if root.nondefault {
        vec![(
            OsString::from(CONFIG_DIR_ENV),
            root.effective_root.clone().into_os_string(),
        )]
    } else {
        Vec::new()
    };

    Ok(ResumeSpec {
        program: OsString::from(AGENT),
        argv,
        cwd: workspace.to_path_buf(),
        env,
    })
}

/// Check whether a filename UUID and an embedded sessionId agree. Comparison
/// is case-insensitive and ignores surrounding braces.
pub(super) fn uuid_agrees(filename: &str, embedded: &str) -> bool {
    normalize_uuid(filename) == normalize_uuid(embedded)
}

/// Normalize a UUID string for comparison: strip `{` `}` and lowercase.
fn normalize_uuid(value: &str) -> String {
    value
        .trim_matches(|c| c == '{' || c == '}')
        .to_ascii_lowercase()
}

/// Validate that a string looks like a UUID (8-4-4-4-12 hex).
pub(super) fn looks_like_uuid(value: &str) -> bool {
    let trimmed = value.trim_matches(|c| c == '{' || c == '}');
    let groups = [8usize, 4, 4, 4, 12];
    let mut idx = 0;
    for (i, &expected) in groups.iter().enumerate() {
        let chunk = match trimmed.get(idx..idx + expected) {
            Some(chunk) => chunk,
            None => return false,
        };
        if !chunk.bytes().all(|b| b.is_ascii_hexdigit()) {
            return false;
        }
        idx += expected;
        if i + 1 < groups.len() {
            match trimmed.as_bytes().get(idx) {
                Some(b'-') => idx += 1,
                _ => return false,
            }
        }
    }
    idx == trimmed.len()
}

#[cfg(test)]
#[path = "tests/resume.rs"]
mod tests;
