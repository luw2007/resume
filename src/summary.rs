//! Deterministic summaries with bounded early read and Unicode display-width
//! truncation.
//!
//! Produces a short, deterministic summary from user messages. The summary is
//! derived from the first available user message text, bounded to a 1 MiB
//! early read, and truncated to a Unicode display width.
//!
//! Slash-command and automated-message filtering is deliberately NOT
//! implemented here: that is integration-specific and delegated to the
//! integrations layer.

use std::io::Read;

use crate::{injection::collapse_known_injections, text};

/// Maximum bytes to read from the source for summary generation.
const SUMMARY_READ_LIMIT: u64 = 1024 * 1024; // 1 MiB

/// Default maximum Unicode display width for a summary.
const DEFAULT_SUMMARY_WIDTH: usize = 80;

/// Build a deterministic summary from an iterator of user message texts.
///
/// - Takes the first non-empty message after normalization and injection
///   filtering.
/// - Truncates to `max_width` Unicode display columns.
/// - Returns `None` if no suitable text is available.
pub fn summarize_texts<'a>(
    texts: impl IntoIterator<Item = &'a str>,
    max_width: usize,
) -> Option<String> {
    for raw in texts {
        let collapsed = collapse_known_injections(raw);
        let normalized = text::normalize(&collapsed, text::Mode::Normalized);
        let trimmed = normalized.trim();
        if !trimmed.is_empty() {
            return Some(text::truncate_to_width(trimmed, max_width));
        }
    }
    None
}

/// Build a summary with default width.
pub fn summarize(texts: impl IntoIterator<Item = String>) -> Option<String> {
    let owned: Vec<String> = texts.into_iter().collect();
    let refs: Vec<&str> = owned.iter().map(String::as_str).collect();
    summarize_texts(refs, DEFAULT_SUMMARY_WIDTH)
}

/// Read up to 1 MiB from a reader for summary purposes. Returns the text
/// (lossy UTF-8) and whether the read was truncated.
pub fn read_bounded_for_summary<R: Read>(reader: &mut R) -> std::io::Result<(String, bool)> {
    let mut buf = Vec::new();
    // Read up to the limit using the original reader, then probe for more.
    let mut limited = (&mut *reader).take(SUMMARY_READ_LIMIT);
    limited.read_to_end(&mut buf)?;
    // Check if the underlying reader still has data beyond the limit.
    let truncated = buf.len() as u64 >= SUMMARY_READ_LIMIT && {
        let mut probe = [0u8; 1];
        reader.read(&mut probe).ok().map(|n| n > 0).unwrap_or(false)
    };
    let text = String::from_utf8_lossy(&buf).into_owned();
    Ok((text, truncated))
}

/// Read a bounded prefix of a file for summary.
pub fn read_file_for_summary(path: &std::path::Path) -> std::io::Result<(String, bool)> {
    let mut file = std::fs::File::open(path)?;
    read_bounded_for_summary(&mut file)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summarizes_first_non_empty_message() {
        let texts = vec![
            "".to_string(),
            "  \n  ".to_string(),
            "Fix the bug".to_string(),
        ];
        assert_eq!(summarize(texts).as_deref(), Some("Fix the bug"));
    }

    #[test]
    fn truncates_long_message_to_display_width() {
        let long = "x".repeat(200);
        let summary = summarize_texts([long.as_str()], 10).unwrap();
        assert!(summary.ends_with('…'));
        use unicode_width::UnicodeWidthStr;
        assert!(summary.width() <= 10);
    }

    #[test]
    fn returns_none_when_all_empty() {
        assert_eq!(summarize(vec!["".to_string(), "   ".to_string()]), None);
        assert_eq!(summarize(Vec::<String>::new()), None);
    }

    #[test]
    fn normalizes_newlines_and_strips_terminal_controls() {
        let texts = vec!["line1\nline2\x1b[31m\x1b[0m".to_string()];
        let summary = summarize(texts).unwrap();
        assert_eq!(summary, "line1 line2");
    }

    #[test]
    fn collapses_known_injection_before_summary() {
        let texts = vec!["<skill>injected</skill> real".to_string()];
        assert_eq!(summarize(texts).as_deref(), Some("injected real"));
    }

    #[test]
    fn read_bounded_truncates_large_input() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.txt");
        // Write 2 MiB.
        let big = "a".repeat(2 * 1024 * 1024);
        std::fs::write(&path, &big).unwrap();
        let (text, truncated) = read_file_for_summary(&path).unwrap();
        assert!(truncated, "large input should be truncated");
        assert!(
            text.len() <= (SUMMARY_READ_LIMIT as usize) + 1,
            "read should be bounded near the limit"
        );
    }

    #[test]
    fn read_bounded_small_input_not_truncated() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("small.txt");
        std::fs::write(&path, "hello").unwrap();
        let (text, truncated) = read_file_for_summary(&path).unwrap();
        assert!(!truncated);
        assert_eq!(text, "hello");
    }

    #[test]
    fn cjk_summary_uses_display_width() {
        // Each CJK char is width 2; with width 10, we fit 4 chars + ellipsis.
        let summary = summarize_texts(["一二三四五六七八九十"], 10).unwrap();
        use unicode_width::UnicodeWidthStr;
        assert!(summary.width() <= 10);
        assert!(summary.ends_with('…'));
    }
}
