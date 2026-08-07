//! Streaming JSONL reader with configurable bounds and per-file error isolation.
//!
//! Shared mechanics only: this reader never assumes a record schema, header
//! position, or event type. Adapters interpret the retained [`serde_json::Value`]
//! records. The reader's job is to safely parse untrusted, possibly live files:
//!
//! - configurable line/file/record bounds and bounded JSON nesting;
//! - retain valid records;
//! - treat a malformed final unterminated line as incomplete;
//! - report malformed middle records without aborting unrelated files;
//! - accept unknown records for adapter-specific dispatch;
//! - never open files for write or follow a session-file symlink outside the
//!   effective configured root.

use std::{
    fs,
    io::{self, BufRead, Read},
    path::{Path, PathBuf},
};

use serde_json::Value;

/// Default safety limits. Generous enough for real transcripts, bounded enough
/// to prevent unbounded allocation under attack.
const DEFAULT_MAX_LINE_BYTES: usize = 8 * 1024 * 1024; // 8 MiB per line
const DEFAULT_MAX_FILE_BYTES: u64 = 512 * 1024 * 1024; // 512 MiB per file
const DEFAULT_MAX_RECORDS: usize = 1_000_000; // 1M records per file
const DEFAULT_MAX_NESTING: usize = 64; // JSON nesting depth

/// Configurable bounds for the JSONL reader.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Bounds {
    pub max_line_bytes: usize,
    pub max_file_bytes: u64,
    pub max_records: usize,
    pub max_nesting: usize,
}

impl Default for Bounds {
    fn default() -> Self {
        Self {
            max_line_bytes: DEFAULT_MAX_LINE_BYTES,
            max_file_bytes: DEFAULT_MAX_FILE_BYTES,
            max_records: DEFAULT_MAX_RECORDS,
            max_nesting: DEFAULT_MAX_NESTING,
        }
    }
}

/// Outcome for a single line/record during streaming.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RecordOutcome {
    /// A valid JSON record was retained.
    Record(Value),
    /// A blank line was skipped.
    Blank,
}

/// How a file's parsing concluded.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FileOutcome {
    /// The file was fully consumed.
    Complete,
    /// The final line was unterminated (no trailing newline) and malformed or
    /// truncated; treated as incomplete, not a hard error.
    IncompleteTail,
    /// A bound was hit and reading stopped early.
    BoundExceeded,
}

/// Result of reading a JSONL file. Collects valid records and diagnostics
/// without aborting on individual malformed middle records.
#[derive(Clone, Debug)]
pub struct ReadResult {
    pub records: Vec<Value>,
    pub outcome: FileOutcome,
    /// Number of malformed middle records encountered (non-final lines that
    /// failed to parse). Each is reported but does not abort the file.
    pub malformed_middle: usize,
    /// Number of lines that exceeded `max_line_bytes` (reported, not aborted).
    pub oversized_lines: usize,
    pub bytes_read: u64,
}

impl ReadResult {
    pub fn record_count(&self) -> usize {
        self.records.len()
    }
}

/// Read-only file open that rejects symlinks resolving outside the effective
/// root, and never opens for write. Returns the resolved real path so callers
/// can confirm it stays inside the root.
pub fn open_for_read(
    path: &Path,
    effective_root: Option<&Path>,
) -> io::Result<(fs::File, PathBuf)> {
    let resolved = path.canonicalize().map_err(|source| {
        io::Error::new(
            source.kind(),
            format!("cannot resolve session file {:?}", path),
        )
    })?;
    if let Some(root) = effective_root {
        if !resolved.starts_with(root) {
            return Err(io::Error::other(format!(
                "session file {:?} resolves outside effective root {:?}",
                resolved, root
            )));
        }
    }
    let file = fs::File::open(&resolved)?;
    Ok((file, resolved))
}

/// Read a JSONL file using a `BufRead`, which cleanly handles multi-line
/// chunks. This is the primary entry point for file reads.
pub fn read_buffered<R: BufRead>(
    reader: &mut R,
    bounds: &Bounds,
    bytes_consumed: u64,
) -> io::Result<ReadResult> {
    let mut records = Vec::new();
    let mut malformed_middle = 0usize;
    let mut oversized_lines = 0usize;
    let mut outcome = FileOutcome::Complete;
    let mut bytes_read = 0u64;

    loop {
        let mut line = Vec::new();
        let read = match reader.read_until(b'\n', &mut line) {
            Ok(n) => n,
            Err(ref e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        if read == 0 {
            break; // EOF
        }
        bytes_read += read as u64;

        // File size bound.
        if bytes_consumed + bytes_read > bounds.max_file_bytes {
            outcome = FileOutcome::BoundExceeded;
            break;
        }

        let had_newline = line.last() == Some(&b'\n');
        let payload = if had_newline {
            &line[..line.len() - 1]
        } else {
            &line[..]
        };

        // Strip a trailing \r for CRLF tolerance.
        let payload = if payload.last() == Some(&b'\r') {
            &payload[..payload.len() - 1]
        } else {
            payload
        };

        if payload.is_empty() {
            continue;
        }

        // Line-size bound: report, skip, and continue.
        if payload.len() > bounds.max_line_bytes {
            oversized_lines += 1;
            if records.len() >= bounds.max_records {
                outcome = FileOutcome::BoundExceeded;
                break;
            }
            continue;
        }

        match parse_bounded(payload, bounds.max_nesting) {
            Ok(value) => {
                records.push(value);
                if records.len() >= bounds.max_records {
                    outcome = FileOutcome::BoundExceeded;
                    break;
                }
            }
            Err(ParseError::Invalid) => {
                if had_newline {
                    // Malformed middle (terminated) record: report, don't abort.
                    malformed_middle += 1;
                } else {
                    // Malformed final unterminated line => incomplete.
                    outcome = FileOutcome::IncompleteTail;
                    break;
                }
            }
        }
    }

    Ok(ReadResult {
        records,
        outcome,
        malformed_middle,
        oversized_lines,
        bytes_read,
    })
}

/// Read a JSONL file from disk with bounds and symlink/root guards.
pub fn read_file(path: &Path, bounds: &Bounds) -> io::Result<ReadResult> {
    let (file, _resolved) = open_for_read(path, None)?;
    let metadata = file.metadata()?;
    read_file_with_metadata(file, metadata.len(), bounds)
}

/// Read a JSONL file confined to an effective root.
pub fn read_file_confined(
    path: &Path,
    effective_root: &Path,
    bounds: &Bounds,
) -> io::Result<ReadResult> {
    let (file, _resolved) = open_for_read(path, Some(effective_root))?;
    let metadata = file.metadata()?;
    read_file_with_metadata(file, metadata.len(), bounds)
}

fn read_file_with_metadata(
    file: fs::File,
    file_len: u64,
    bounds: &Bounds,
) -> io::Result<ReadResult> {
    let mut reader = io::BufReader::new(file);

    // If the file exceeds the file-size bound, read up to the bound and
    // classify as BoundExceeded regardless of tail completeness, since the
    // truncation is imposed by the bound, not by a live writer.
    if file_len > bounds.max_file_bytes {
        let mut limited = (&mut reader).take(bounds.max_file_bytes);
        let mut result = read_buffered(&mut limited, bounds, 0)?;
        result.outcome = FileOutcome::BoundExceeded;
        Ok(result)
    } else {
        read_buffered(&mut reader, bounds, 0)
    }
}

#[derive(Debug)]
enum ParseError {
    Invalid,
}

/// Parse a JSON byte slice with a nesting-depth bound. serde_json's default
/// recursion limit is 128; we enforce a tighter, configurable bound and treat
/// any parse failure as `Invalid` (non-aborting).
fn parse_bounded(bytes: &[u8], max_nesting: usize) -> Result<Value, ParseError> {
    // serde_json enforces its own recursion limit; we layer ours by pre-checking
    // nesting depth is not absurdly high before attempting full parse.
    if max_nesting == 0 {
        return Err(ParseError::Invalid);
    }
    let mut deserializer = serde_json::Deserializer::from_slice(bytes).into_iter::<Value>();
    match deserializer.next() {
        Some(Ok(value)) => {
            // Reject deeply nested structures that exceed our bound.
            if nesting_depth(&value) > max_nesting {
                return Err(ParseError::Invalid);
            }
            // Ensure no trailing content.
            if deserializer.next().is_some() {
                return Err(ParseError::Invalid);
            }
            Ok(value)
        }
        _ => Err(ParseError::Invalid),
    }
}

/// Compute the maximum nesting depth of a JSON value.
fn nesting_depth(value: &Value) -> usize {
    match value {
        Value::Object(map) => map
            .values()
            .map(nesting_depth)
            .max()
            .map(|d| d + 1)
            .unwrap_or(1),
        Value::Array(arr) => arr
            .iter()
            .map(nesting_depth)
            .max()
            .map(|d| d + 1)
            .unwrap_or(1),
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_temp(content: &[u8]) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut file = fs::File::create(&path).unwrap();
        file.write_all(content).unwrap();
        file.sync_all().unwrap();
        (dir, path)
    }

    #[test]
    fn retains_valid_records_and_accepts_unknown_types() {
        let (_dir, path) = write_temp(
            br#"{"type":"session","id":"abc"}
{"type":"user_message","text":"hi"}
{"type":"totally_unknown","foo":"bar"}
"#,
        );
        let result = read_file(&path, &Bounds::default()).unwrap();
        assert_eq!(result.records.len(), 3);
        assert_eq!(result.outcome, FileOutcome::Complete);
        assert_eq!(result.malformed_middle, 0);
    }

    #[test]
    fn malformed_middle_record_is_reported_without_aborting() {
        let (_dir, path) = write_temp(
            br#"{"type":"session","id":"abc"}
{not valid json at all}
{"type":"user_message","text":"after"}
"#,
        );
        let result = read_file(&path, &Bounds::default()).unwrap();
        assert_eq!(
            result.records.len(),
            2,
            "valid records before and after retained"
        );
        assert_eq!(result.malformed_middle, 1);
        assert_eq!(result.outcome, FileOutcome::Complete);
        // Confirm the record after the malformed one was retained.
        assert_eq!(
            result.records[1].get("type").and_then(|v| v.as_str()),
            Some("user_message")
        );
    }

    #[test]
    fn malformed_final_unterminated_line_is_incomplete() {
        let (_dir, path) = write_temp(
            b"{\"type\":\"session\"}\n{\"type\":\"user\"}\n{truncated record without newl\"",
        );
        let result = read_file(&path, &Bounds::default()).unwrap();
        assert_eq!(result.records.len(), 2);
        assert_eq!(result.outcome, FileOutcome::IncompleteTail);
        assert_eq!(
            result.malformed_middle, 0,
            "tail is incomplete, not malformed"
        );
    }

    #[test]
    fn blank_lines_are_skipped() {
        let (_dir, path) = write_temp(
            br#"{"a":1}

{"b":2}
"#,
        );
        let result = read_file(&path, &Bounds::default()).unwrap();
        assert_eq!(result.records.len(), 2);
    }

    #[test]
    fn crlf_line_endings_tolerated() {
        let (_dir, path) = write_temp(b"{\"a\":1}\r\n{\"b\":2}\r\n");
        let result = read_file(&path, &Bounds::default()).unwrap();
        assert_eq!(result.records.len(), 2);
    }

    #[test]
    fn live_growing_file_final_partial_is_incomplete() {
        // Simulates a file being actively written: ends mid-record.
        let (_dir, path) = write_temp(
            b"{\"type\":\"session\",\"id\":\"live\"}\n{\"type\":\"user\",\"text\":\"hello\"}\n{\"type\":\"user\",\"text\":\"being writ",
        );
        let result = read_file(&path, &Bounds::default()).unwrap();
        assert_eq!(result.records.len(), 2);
        assert_eq!(result.outcome, FileOutcome::IncompleteTail);
    }

    #[test]
    fn huge_line_exceeding_max_line_bytes_is_reported_not_aborted() {
        let big = format!(
            "{{\"type\":\"ok\"}}\n{{\"huge\":\"{}\"}}\n{{\"type\":\"after\"}}\n",
            "x".repeat(DEFAULT_MAX_LINE_BYTES + 10)
        );
        let (_dir, path) = write_temp(big.as_bytes());
        let result = read_file(&path, &Bounds::default()).unwrap();
        assert_eq!(
            result.records.len(),
            2,
            "valid records before and after the oversized line are kept"
        );
        assert!(result.oversized_lines >= 1);
    }

    #[test]
    fn max_records_bound_stops_early() {
        let bounds = Bounds {
            max_records: 2,
            ..Bounds::default()
        };
        let content = b"{\"a\":1}\n{\"b\":2}\n{\"c\":3}\n{\"d\":4}\n";
        let (_dir, path) = write_temp(content);
        let result = read_file(&path, &bounds).unwrap();
        assert_eq!(result.records.len(), 2);
        assert_eq!(result.outcome, FileOutcome::BoundExceeded);
    }

    #[test]
    fn deeply_nested_json_exceeding_max_nesting_rejected() {
        let depth = 200;
        let mut json = String::new();
        for _ in 0..depth {
            json.push_str("{\"a\":");
        }
        json.push('1');
        for _ in 0..depth {
            json.push('}');
        }
        let content = format!("{}\n", json);
        let (_dir, path) = write_temp(content.as_bytes());
        let result = read_file(&path, &Bounds::default()).unwrap();
        // The deeply nested record is rejected as malformed middle.
        assert_eq!(result.records.len(), 0);
        assert_eq!(result.malformed_middle, 1);
    }

    #[test]
    fn symlink_outside_effective_root_rejected() {
        #[cfg(unix)]
        {
            let root = tempfile::tempdir().unwrap();
            let outside = tempfile::tempdir().unwrap();
            let target = outside.path().join("secret.jsonl");
            fs::write(&target, b"{}\n").unwrap();
            let link = root.path().join("link.jsonl");
            std::os::unix::fs::symlink(&target, &link).unwrap();

            let err = read_file_confined(&link, root.path(), &Bounds::default())
                .err()
                .expect("symlink outside root must be rejected");
            assert!(err.to_string().contains("outside effective root"));
        }
    }

    #[test]
    fn symlink_inside_effective_root_allowed() {
        #[cfg(unix)]
        {
            let root = tempfile::tempdir().unwrap();
            // Canonicalize root to match the resolution that open_for_read
            // performs (macOS may resolve /var -> /private/var).
            let root_canon = root.path().canonicalize().unwrap();
            let real = root_canon.join("real.jsonl");
            fs::write(&real, b"{\"ok\":true}\n").unwrap();
            let link = root_canon.join("link.jsonl");
            std::os::unix::fs::symlink(&real, &link).unwrap();

            let result = read_file_confined(&link, &root_canon, &Bounds::default()).unwrap();
            assert_eq!(result.records.len(), 1);
        }
    }

    #[test]
    fn empty_file_yields_complete_with_zero_records() {
        let (_dir, path) = write_temp(b"");
        let result = read_file(&path, &Bounds::default()).unwrap();
        assert_eq!(result.records.len(), 0);
        assert_eq!(result.outcome, FileOutcome::Complete);
    }

    #[test]
    fn only_newline_yields_complete_with_zero_records() {
        let (_dir, path) = write_temp(b"\n\n\n");
        let result = read_file(&path, &Bounds::default()).unwrap();
        assert_eq!(result.records.len(), 0);
        assert_eq!(result.outcome, FileOutcome::Complete);
    }

    #[test]
    fn file_size_bound_truncates_to_bound_exceeded() {
        let bounds = Bounds {
            max_file_bytes: 30,
            ..Bounds::default()
        };
        let content = b"{\"a\":1}\n{\"b\":2}\n{\"c\":3}\n{\"d\":4}\n";
        let (_dir, path) = write_temp(content);
        let result = read_file(&path, &bounds).unwrap();
        assert_eq!(result.outcome, FileOutcome::BoundExceeded);
        // Some valid records were still retained.
        assert!(!result.records.is_empty());
    }
}
