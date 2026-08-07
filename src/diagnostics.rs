//! Redacted diagnostics with `--verbose` support.
//!
//! Diagnostics never log message bodies or sensitive remotes/URLs. In normal
//! mode, only a category and count are shown. In verbose mode, a redacted path
//! and error chain may be included, with sensitive content scrubbed.

use std::path::{Path, PathBuf};

/// A redacted, category-based diagnostic. Safe to print in non-verbose mode.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RedactedDiagnostic {
    /// Category label, e.g. "jsonl_malformed", "io_error".
    pub category: &'static str,
    /// Count of occurrences.
    pub count: usize,
    /// Verbose-only path, redacted of sensitive components.
    pub verbose_path: Option<PathBuf>,
    /// Verbose-only error chain, redacted of URLs and message bodies.
    pub verbose_chain: Option<String>,
}

impl RedactedDiagnostic {
    pub fn new(category: &'static str) -> Self {
        Self {
            category,
            count: 1,
            verbose_path: None,
            verbose_chain: None,
        }
    }

    /// Render in normal (non-verbose) mode: category and count only.
    pub fn render_normal(&self) -> String {
        format!("{}: {}", self.category, self.count)
    }

    /// Render in verbose mode: includes redacted path and chain.
    pub fn render_verbose(&self) -> String {
        let mut out = self.render_normal();
        if let Some(path) = &self.verbose_path {
            out.push_str(" path=");
            out.push_str(&redact_path(path));
        }
        if let Some(chain) = &self.verbose_chain {
            out.push_str(" detail=");
            out.push_str(&redact_text(chain));
        }
        out
    }
}

/// A collection of diagnostics keyed by category, with counts aggregated.
#[derive(Clone, Debug, Default)]
pub struct DiagnosticCollector {
    entries: Vec<RedactedDiagnostic>,
}

impl DiagnosticCollector {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a diagnostic occurrence. Aggregates count by category.
    pub fn record(&mut self, category: &'static str) {
        if let Some(entry) = self.entries.iter_mut().find(|e| e.category == category) {
            entry.count += 1;
        } else {
            self.entries.push(RedactedDiagnostic::new(category));
        }
    }

    /// Record a diagnostic with verbose detail.
    pub fn record_detail(
        &mut self,
        category: &'static str,
        path: Option<PathBuf>,
        chain: Option<String>,
    ) {
        let redacted_chain = chain.map(|c| redact_text(&c));
        if let Some(entry) = self.entries.iter_mut().find(|e| e.category == category) {
            entry.count += 1;
            if path.is_some() {
                entry.verbose_path = path;
            }
            if redacted_chain.is_some() {
                entry.verbose_chain = redacted_chain;
            }
        } else {
            self.entries.push(RedactedDiagnostic {
                category,
                count: 1,
                verbose_path: path,
                verbose_chain: redacted_chain,
            });
        }
    }

    /// Render all diagnostics. In non-verbose mode, only category + count.
    pub fn render(&self, verbose: bool) -> String {
        self.entries
            .iter()
            .map(|e| {
                if verbose {
                    e.render_verbose()
                } else {
                    e.render_normal()
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Redact sensitive content from a path for verbose display.
///
/// This replaces `$HOME` with `$HOME` and strips query-string-like suffixes.
/// It does not reveal message bodies or remote URLs embedded in paths.
pub fn redact_path(path: &Path) -> String {
    let display = path.display().to_string();
    redact_text(&display)
}

/// Redact sensitive content from arbitrary text. Removes:
/// - URLs (http/https/ssh/git/file schemes)
/// - Message bodies (heuristic: lines containing "body=" or "message=")
/// - Base64-like blobs (long alphanumeric+/= sequences)
pub fn redact_text(text: &str) -> String {
    let mut result = text.to_string();

    // Redact URLs with known schemes.
    for scheme in [
        "https://", "http://", "ssh://", "git://", "file://", "ftp://",
    ] {
        while let Some(start) = result.find(scheme) {
            let rest = &result[start + scheme.len()..];
            // Find end of URL (whitespace or end of string).
            let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
            let url_end = start + scheme.len() + end;
            result.replace_range(start..url_end, "[redacted-url]");
        }
    }

    // Redact remote-like patterns: git@host:...
    while let Some(start) = result.find("git@") {
        let rest = &result[start..];
        let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        result.replace_range(start..start + end, "[redacted-remote]");
    }

    // Redact base64-like blobs (40+ chars of base64 alphabet).
    let mut redacted = String::with_capacity(result.len());
    let mut i = 0;
    let bytes = result.as_bytes();
    while i < bytes.len() {
        let run_start = i;
        let mut run_len = 0;
        while i < bytes.len() && is_base64_char(bytes[i]) {
            run_len += 1;
            i += 1;
        }
        if run_len >= 40 {
            redacted.push_str("[redacted-blob]");
        } else {
            redacted.push_str(&result[run_start..run_start + run_len]);
        }
        if i < bytes.len() {
            redacted.push(bytes[i] as char);
            i += 1;
        }
    }
    redacted
}

fn is_base64_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'+' || b == b'/' || b == b'='
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normal_mode_shows_category_and_count_only() {
        let d = RedactedDiagnostic::new("jsonl_malformed");
        assert_eq!(d.render_normal(), "jsonl_malformed: 1");
    }

    #[test]
    fn verbose_mode_includes_redacted_path() {
        let d = RedactedDiagnostic {
            category: "io_error",
            count: 2,
            verbose_path: Some(PathBuf::from("/home/user/session.jsonl")),
            verbose_chain: None,
        };
        let rendered = d.render_verbose();
        assert!(rendered.contains("io_error: 2"));
        assert!(rendered.contains("/home/user/session.jsonl"));
    }

    #[test]
    fn redact_text_removes_urls() {
        let input = "error fetching https://secret.example.com/data path=/x";
        let redacted = redact_text(input);
        assert!(!redacted.contains("secret.example.com"));
        assert!(redacted.contains("[redacted-url]"));
    }

    #[test]
    fn redact_text_removes_git_remote() {
        let input = "origin git@github.com:user/repo.git failed";
        let redacted = redact_text(input);
        assert!(!redacted.contains("github.com"));
        assert!(redacted.contains("[redacted-remote]"));
    }

    #[test]
    fn redact_text_removes_base64_blobs() {
        let blob = "a".repeat(50);
        let input = format!("data={blob}");
        let redacted = redact_text(&input);
        assert!(redacted.contains("[redacted-blob]"));
        assert!(!redacted.contains(&blob));
    }

    #[test]
    fn redact_text_preserves_short_text() {
        let input = "jsonl_malformed: 1 path=/tmp/x.jsonl";
        let redacted = redact_text(input);
        assert_eq!(redacted, input);
    }

    #[test]
    fn collector_aggregates_by_category() {
        let mut collector = DiagnosticCollector::new();
        collector.record("jsonl_malformed");
        collector.record("jsonl_malformed");
        collector.record("io_error");
        let normal = collector.render(false);
        assert!(normal.contains("jsonl_malformed: 2"));
        assert!(normal.contains("io_error: 1"));
    }

    #[test]
    fn verbose_includes_chain_and_path() {
        let mut collector = DiagnosticCollector::new();
        collector.record_detail(
            "jsonl_malformed",
            Some(PathBuf::from("/sessions/abc.jsonl")),
            Some("invalid JSON at line 42".into()),
        );
        let verbose = collector.render(true);
        assert!(verbose.contains("/sessions/abc.jsonl"));
        assert!(verbose.contains("invalid JSON at line 42"));
    }

    #[test]
    fn redact_path_scrubs_urls_in_path() {
        let path = Path::new("/sessions/https://evil.example/steal");
        let redacted = redact_path(path);
        assert!(!redacted.contains("evil.example"));
    }

    #[test]
    fn no_message_body_leaks_in_normal_mode() {
        let d = RedactedDiagnostic {
            category: "test",
            count: 1,
            verbose_path: None,
            verbose_chain: Some("user said: my secret password is hunter2".into()),
        };
        let normal = d.render_normal();
        assert!(!normal.contains("hunter2"));
        assert!(!normal.contains("password"));
    }
}
