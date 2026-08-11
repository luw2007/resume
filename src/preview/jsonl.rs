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
    if let Some(root) = effective_root
        && !resolved.starts_with(root)
    {
        return Err(io::Error::other(format!(
            "session file {:?} resolves outside effective root {:?}",
            resolved, root
        )));
    }
    let file = fs::File::open(&resolved)?;
    Ok((file, resolved))
}

/// Read one physical line from `reader`, buffering at most `cap` bytes of
/// its payload regardless of the line's true length — the security property
/// `read_buffered`'s line-size bound depends on. `read_until` alone cannot
/// provide this: it grows its buffer to the full line before returning, so
/// checking the length only afterward still lets an attacker force an
/// allocation proportional to an arbitrarily large single line. This drains
/// any bytes past `cap` without appending them, tracking the *true* payload
/// length (`payload_len`, excluding the trailing `\n`) separately so the
/// caller can still correctly detect and count an oversized line. Returns
/// `None` at true EOF (nothing left to read); otherwise
/// `Some((buffered_prefix, true_payload_len, found_newline))`.
fn read_line_capped(
    reader: &mut impl BufRead,
    cap: usize,
) -> io::Result<Option<(Vec<u8>, usize, bool)>> {
    let mut line = Vec::new();
    let mut payload_len = 0usize;
    let mut read_any = false;
    loop {
        let buf = match reader.fill_buf() {
            Ok(buf) => buf,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        if buf.is_empty() {
            return Ok(if read_any { Some((line, payload_len, false)) } else { None });
        }
        read_any = true;
        if let Some(pos) = buf.iter().position(|&b| b == b'\n') {
            let keep = cap.saturating_sub(line.len()).min(pos);
            line.extend_from_slice(&buf[..keep]);
            payload_len += pos;
            reader.consume(pos + 1);
            return Ok(Some((line, payload_len, true)));
        }
        let keep = cap.saturating_sub(line.len()).min(buf.len());
        line.extend_from_slice(&buf[..keep]);
        payload_len += buf.len();
        let n = buf.len();
        reader.consume(n);
    }
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
        // Cap at max_line_bytes + 1 so a line of exactly the bound doesn't
        // get truncated before the oversized check below can see it's fine.
        let Some((line, payload_len, had_newline)) =
            read_line_capped(reader, bounds.max_line_bytes.saturating_add(1))?
        else {
            break; // EOF
        };
        bytes_read += payload_len as u64 + u64::from(had_newline);

        // File size bound.
        if bytes_consumed + bytes_read > bounds.max_file_bytes {
            outcome = FileOutcome::BoundExceeded;
            break;
        }

        // Strip a trailing \r for CRLF tolerance (the buffered prefix
        // already excludes the \n that `read_line_capped` consumed).
        let payload: &[u8] = if line.last() == Some(&b'\r') {
            &line[..line.len() - 1]
        } else {
            &line[..]
        };

        if payload.is_empty() && payload_len == 0 {
            continue;
        }

        // Line-size bound: `payload_len` is the *true* length (never
        // truncated by the cap above), so this correctly flags a
        // capped-but-still-oversized line without ever having fully
        // buffered it.
        if payload_len > bounds.max_line_bytes {
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
    use std::{
        io::Write,
        sync::{Arc, Barrier},
        thread,
        time::Duration,
    };

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
    fn concurrent_writer_append_is_read_safely() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("live.jsonl");
        fs::write(&path, b"{\"seq\":0}\n").unwrap();
        let start = Arc::new(Barrier::new(2));
        let writer_path = path.clone();
        let writer_start = Arc::clone(&start);
        let writer = thread::spawn(move || {
            let mut file = fs::OpenOptions::new()
                .append(true)
                .open(writer_path)
                .unwrap();
            writer_start.wait();
            for seq in 1..=32 {
                writeln!(file, "{{\"seq\":{seq}}}").unwrap();
                file.flush().unwrap();
                thread::sleep(Duration::from_millis(1));
            }
        });
        start.wait();

        // Re-open on every read, as discovery does. Every snapshot must retain
        // the complete prefix and may only classify the concurrently-written
        // tail as incomplete; it must never invent or reorder records.
        for _ in 0..32 {
            let result = read_file(
                &path,
                &Bounds {
                    max_line_bytes: 1024,
                    max_file_bytes: 64 * 1024,
                    max_records: 64,
                    max_nesting: 8,
                },
            )
            .unwrap();
            let sequences: Vec<u64> = result
                .records
                .iter()
                .filter_map(|record| record.get("seq").and_then(Value::as_u64))
                .collect();
            assert_eq!(sequences.first(), Some(&0));
            assert!(sequences.windows(2).all(|pair| pair[1] == pair[0] + 1));
            thread::sleep(Duration::from_millis(1));
        }
        writer.join().unwrap();
        let final_read = read_file(&path, &Bounds::default()).unwrap();
        assert_eq!(final_read.record_count(), 33);
        assert_eq!(final_read.outcome, FileOutcome::Complete);
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
    fn read_line_capped_never_buffers_past_the_cap() {
        // The bug this guards: `read_until` grows its buffer to the full
        // line before any size check runs, so an attacker-controlled line
        // far larger than `max_line_bytes` still forces an allocation of
        // its own full length. `read_line_capped` must never buffer more
        // than `cap` bytes, regardless of how much larger the true line is,
        // while still correctly reporting the true length so the caller can
        // flag it oversized.
        let cap = 64usize;
        let true_len = cap * 1000; // far larger than the cap
        let mut line = "y".repeat(true_len).into_bytes();
        line.push(b'\n');
        line.extend_from_slice(b"{\"type\":\"after\"}\n");
        let mut reader = std::io::Cursor::new(line);

        let (buffered, payload_len, found_newline) =
            read_line_capped(&mut reader, cap).unwrap().unwrap();
        assert!(
            buffered.len() <= cap,
            "buffered {} bytes for a cap of {cap}",
            buffered.len()
        );
        assert_eq!(payload_len, true_len, "true length must still be reported");
        assert!(found_newline);

        // The reader must be correctly positioned past the oversized line's
        // newline, ready to read the next line normally.
        let (next, next_len, next_newline) =
            read_line_capped(&mut reader, cap).unwrap().unwrap();
        assert_eq!(next, b"{\"type\":\"after\"}");
        assert_eq!(next_len, next.len());
        assert!(next_newline);
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
                .expect_err("symlink outside root must be rejected");
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
