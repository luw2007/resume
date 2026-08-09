//! Codex activity detection (positive evidence only).
//!
//! One process-first probe indexes rollout files held open by Codex processes.
//! An exact path or confirmed device/inode match is Active; missing evidence is
//! Unknown, never Inactive. The immutable snapshot is shared by all discovery
//! workers, keeping subprocess use constant regardless of session count.

use std::{
    collections::HashMap,
    ffi::{OsStr, OsString},
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::SystemTime,
};

use crate::session::{ActivityStatus, Diagnostic};

use super::roots::is_rollout_filename;

pub(crate) const PROBE_PROGRAM: &str = "lsof";
pub(crate) const CODEX_COMMAND_PREFIX: &str = "codex";
pub(crate) const PROBE_STAT_TIMEOUT_SECONDS: &str = "2";
pub(crate) const DIAG_PROBE_FAILED: &str = "codex_activity_probe_failed";
pub(crate) const DIAG_PROBE_PARTIAL: &str = "codex_activity_probe_partial";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FdEvidence {
    pub pid: u32,
    pub command: String,
    pub path: PathBuf,
    pub device: Option<u64>,
    pub inode: Option<u64>,
}

#[derive(Clone, Debug)]
pub struct ActivitySnapshot {
    observed_at: SystemTime,
    entries: Vec<FdEvidence>,
    by_path: HashMap<PathBuf, usize>,
    by_identity: HashMap<(u64, u64), Vec<usize>>,
    by_name: HashMap<OsString, Vec<usize>>,
}

impl ActivitySnapshot {
    pub fn empty() -> Self {
        Self::from_entries(Vec::new(), SystemTime::now())
    }

    pub fn observed_at(&self) -> SystemTime {
        self.observed_at
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn lookup(&self, rollout_path: &Path) -> Option<&FdEvidence> {
        if let Some(index) = self.by_path.get(rollout_path) {
            return self.entries.get(*index);
        }
        let candidates = self.by_name.get(rollout_path.file_name()?)?;
        let identity = file_identity(rollout_path)?;
        self.by_identity
            .get(&identity)?
            .iter()
            .find(|index| candidates.contains(index))
            .and_then(|index| self.entries.get(*index))
    }

    fn from_entries(entries: Vec<FdEvidence>, observed_at: SystemTime) -> Self {
        let mut by_path = HashMap::new();
        let mut by_identity: HashMap<(u64, u64), Vec<usize>> = HashMap::new();
        let mut by_name: HashMap<OsString, Vec<usize>> = HashMap::new();
        for (index, entry) in entries.iter().enumerate() {
            by_path.entry(entry.path.clone()).or_insert(index);
            if let (Some(device), Some(inode)) = (entry.device, entry.inode) {
                by_identity.entry((device, inode)).or_default().push(index);
            }
            if let Some(name) = entry.path.file_name() {
                by_name.entry(name.to_owned()).or_default().push(index);
            }
        }
        Self {
            observed_at,
            entries,
            by_path,
            by_identity,
            by_name,
        }
    }
}

pub fn activity_status(rollout_path: &Path, snapshot: Option<&ActivitySnapshot>) -> ActivityStatus {
    match snapshot.and_then(|snapshot| snapshot.lookup(rollout_path)) {
        Some(_) => ActivityStatus::Active {
            observed_at: snapshot.expect("matched snapshot exists").observed_at(),
        },
        None => ActivityStatus::Unknown,
    }
}

pub fn probe() -> (ActivitySnapshot, Vec<Diagnostic>) {
    if crate::proc::proc_probe_disabled(
        std::env::var_os(crate::proc::DISABLE_PROC_PROBE_ENV).as_deref(),
    ) {
        return (ActivitySnapshot::empty(), Vec::new());
    }
    let observed_at = SystemTime::now();
    if crate::launch::command_available(OsStr::new(PROBE_PROGRAM)) {
        return probe_with(OsStr::new(PROBE_PROGRAM), observed_at);
    }
    #[cfg(target_os = "linux")]
    {
        let (entries, skipped) = probe_proc(observed_at);
        let snapshot = ActivitySnapshot::from_entries(entries, observed_at);
        (
            snapshot.clone(),
            partial_diagnostics(&snapshot, skipped, false),
        )
    }
    #[cfg(not(target_os = "linux"))]
    (
        ActivitySnapshot::from_entries(Vec::new(), observed_at),
        Vec::new(),
    )
}

pub(crate) fn probe_with(
    program: &OsStr,
    observed_at: SystemTime,
) -> (ActivitySnapshot, Vec<Diagnostic>) {
    let output = match Command::new(program).args(probe_argv()).output() {
        Ok(output) => output,
        Err(_) => {
            return (
                ActivitySnapshot::from_entries(Vec::new(), observed_at),
                vec![Diagnostic {
                    category: DIAG_PROBE_FAILED,
                    count: 1,
                    verbose_path: None,
                    verbose_chain: Some("unable to run Codex activity probe".into()),
                }],
            );
        }
    };
    let (entries, skipped) = parse_lsof_output(&output.stdout);
    let snapshot = ActivitySnapshot::from_entries(entries, observed_at);
    let diagnostics = partial_diagnostics(&snapshot, skipped, !output.stderr.is_empty());
    (snapshot, diagnostics)
}

fn partial_diagnostics(
    snapshot: &ActivitySnapshot,
    skipped: usize,
    stderr_present: bool,
) -> Vec<Diagnostic> {
    if snapshot.is_empty() || (skipped == 0 && !stderr_present) {
        return Vec::new();
    }
    vec![Diagnostic {
        category: DIAG_PROBE_PARTIAL,
        count: skipped.max(1),
        verbose_path: None,
        verbose_chain: Some("lsof reported unreadable processes; evidence is partial".into()),
    }]
}

pub(crate) fn probe_argv() -> Vec<OsString> {
    [
        "-n",
        "-P",
        "-w",
        "-S",
        PROBE_STAT_TIMEOUT_SECONDS,
        "-F0pcfnDi",
        "-c",
        CODEX_COMMAND_PREFIX,
    ]
    .into_iter()
    .map(OsString::from)
    .collect()
}

#[cfg(target_os = "linux")]
pub(crate) fn probe_proc(_observed_at: SystemTime) -> (Vec<FdEvidence>, usize) {
    use std::os::unix::fs::MetadataExt;

    let mut entries = Vec::new();
    let mut skipped = 0;
    let processes = match fs::read_dir("/proc") {
        Ok(processes) => processes,
        Err(_) => return (entries, 1),
    };
    for process in processes.flatten() {
        let Some(pid) = process
            .file_name()
            .to_str()
            .and_then(|pid| pid.parse().ok())
        else {
            continue;
        };
        let root = process.path();
        let command = match fs::read_to_string(root.join("comm")) {
            Ok(command) if command.trim_end().starts_with(CODEX_COMMAND_PREFIX) => {
                command.trim_end().to_owned()
            }
            Ok(_) => continue,
            Err(_) => {
                skipped += 1;
                continue;
            }
        };
        let descriptors = match fs::read_dir(root.join("fd")) {
            Ok(descriptors) => descriptors,
            Err(_) => {
                skipped += 1;
                continue;
            }
        };
        for descriptor in descriptors.flatten() {
            let path = match fs::read_link(descriptor.path()) {
                Ok(path) if is_rollout_filename(path.file_name()) => path,
                Ok(_) => continue,
                Err(_) => {
                    skipped += 1;
                    continue;
                }
            };
            let metadata = fs::metadata(descriptor.path()).ok();
            entries.push(FdEvidence {
                pid,
                command: command.clone(),
                path,
                device: metadata.as_ref().map(MetadataExt::dev),
                inode: metadata.as_ref().map(MetadataExt::ino),
            });
        }
    }
    (entries, skipped)
}

pub(crate) fn parse_lsof_output(stdout: &[u8]) -> (Vec<FdEvidence>, usize) {
    let mut entries = Vec::new();
    let mut skipped = 0;
    let mut process: Option<(u32, String)> = None;
    for set in stdout
        .split(|byte| *byte == b'\n')
        .filter(|set| !set.is_empty())
    {
        let fields: Vec<&[u8]> = set
            .split(|byte| *byte == b'\0')
            .filter(|field| !field.is_empty())
            .collect();
        let Some(first) = fields.first().and_then(|field| field.first()) else {
            continue;
        };
        match first {
            b'p' => {
                let pid = fields
                    .iter()
                    .find(|field| field.first() == Some(&b'p'))
                    .and_then(|field| std::str::from_utf8(&field[1..]).ok())
                    .and_then(|pid| pid.parse::<u32>().ok());
                let command = fields
                    .iter()
                    .find(|field| field.first() == Some(&b'c'))
                    .and_then(|field| std::str::from_utf8(&field[1..]).ok());
                process = match (pid, command) {
                    (Some(pid), Some(command)) => Some((pid, command.to_owned())),
                    _ => {
                        skipped += 1;
                        None
                    }
                };
            }
            b'f' => {
                let Some((pid, command)) = process.as_ref() else {
                    skipped += 1;
                    continue;
                };
                let field = |tag| fields.iter().find(|field| field.first() == Some(&tag));
                let Some(path) = field(b'n')
                    .and_then(|field| std::str::from_utf8(&field[1..]).ok())
                    .map(PathBuf::from)
                else {
                    skipped += 1;
                    continue;
                };
                if !is_rollout_filename(path.file_name()) {
                    continue;
                }
                let device = field(b'D')
                    .and_then(|field| std::str::from_utf8(&field[1..]).ok())
                    .and_then(parse_device_field);
                let inode = field(b'i')
                    .and_then(|field| std::str::from_utf8(&field[1..]).ok())
                    .and_then(parse_inode_field);
                entries.push(FdEvidence {
                    pid: *pid,
                    command: command.clone(),
                    path,
                    device,
                    inode,
                });
            }
            _ => skipped += 1,
        }
    }
    (entries, skipped)
}

fn parse_device_field(value: &str) -> Option<u64> {
    value
        .strip_prefix("0x")
        .and_then(|value| u64::from_str_radix(value, 16).ok())
}

fn parse_inode_field(value: &str) -> Option<u64> {
    value.parse().ok()
}

fn file_identity(path: &Path) -> Option<(u64, u64)> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        fs::metadata(path)
            .ok()
            .map(|metadata| (metadata.dev(), metadata.ino()))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    #[cfg(unix)]
    use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};

    #[test]
    fn parser_isolates_bad_processes_and_descriptors() {
        let bytes = b"p12\0ccodex\0\nfcwd\0n/tmp/not-a-rollout\0\nf11\0D0x10\0i20\0n/tmp/rollout-good.jsonl\0\npbad\0ccodex\0\nf12\0n/tmp/rollout-lost.jsonl\0\np13\0ccodex-helper\0\nf9\0Dgarbage\0i21\0n/tmp/rollout-second.jsonl\0\nf10\0D0x10\0i22\0\n";
        let (entries, skipped) = parse_lsof_output(bytes);
        assert_eq!(entries.len(), 2);
        assert_eq!(skipped, 3);
        assert_eq!(entries[0].pid, 12);
        assert_eq!(entries[1].device, None);
    }

    #[cfg(unix)]
    #[test]
    fn lookup_handles_symlinked_ancestor_without_basename_false_positive() {
        let temp = tempfile::tempdir().unwrap();
        let real = temp.path().join("real");
        fs::create_dir(&real).unwrap();
        let linked = temp.path().join("linked");
        symlink(&real, &linked).unwrap();
        let rollout = real.join("rollout-shared.jsonl");
        fs::write(&rollout, "{}").unwrap();
        let metadata = fs::metadata(&rollout).unwrap();
        let snapshot = ActivitySnapshot::from_entries(
            vec![FdEvidence {
                pid: 7,
                command: "codex".into(),
                path: rollout.clone(),
                device: Some(metadata.dev()),
                inode: Some(metadata.ino()),
            }],
            SystemTime::UNIX_EPOCH,
        );
        assert!(
            snapshot
                .lookup(&linked.join("rollout-shared.jsonl"))
                .is_some()
        );

        let other = temp.path().join("other");
        fs::create_dir(&other).unwrap();
        let copy = other.join("rollout-shared.jsonl");
        fs::write(&copy, "different").unwrap();
        assert!(snapshot.lookup(&copy).is_none());
        fs::remove_file(&copy).unwrap();
        assert!(snapshot.lookup(&copy).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn lookup_matches_each_duplicate_identity_candidate() {
        let temp = tempfile::tempdir().unwrap();
        let real = temp.path().join("real");
        fs::create_dir(&real).unwrap();
        let first = real.join("rollout-first.jsonl");
        let second = real.join("rollout-second.jsonl");
        fs::write(&first, "{}").unwrap();
        fs::hard_link(&first, &second).unwrap();
        let linked = temp.path().join("linked");
        symlink(&real, &linked).unwrap();
        let metadata = fs::metadata(&first).unwrap();
        let evidence = |path| FdEvidence {
            pid: 7,
            command: "codex".into(),
            path,
            device: Some(metadata.dev()),
            inode: Some(metadata.ino()),
        };
        let snapshot = ActivitySnapshot::from_entries(
            vec![evidence(first), evidence(second)],
            SystemTime::UNIX_EPOCH,
        );

        let matched = snapshot
            .lookup(&linked.join("rollout-second.jsonl"))
            .expect("the second hard-link path should retain identity evidence");
        assert_eq!(matched.path, real.join("rollout-second.jsonl"));
    }

    /// The fake `lsof` replacement writes its NUL-delimited `-F0` output as
    /// raw bytes to a data file via Rust (`fs::write`), then the launched
    /// script `cat`s that file verbatim. This sidesteps shell `printf`
    /// backslash-escape interpretation entirely: `printf`'s `\0`/octal
    /// escape handling is not identical across `/bin/sh` implementations
    /// (dash vs bash-as-sh, BSD vs GNU), so embedding NUL bytes in a shell
    /// script's `printf` argument is not a portable way to build test
    /// fixtures. `cat` has no such ambiguity: it copies the exact bytes
    /// Rust wrote, on any POSIX platform.
    #[cfg(unix)]
    #[test]
    fn one_probe_serves_many_session_lookups() {
        let temp = tempfile::tempdir().unwrap();
        let count = temp.path().join("count");
        let rollout = temp.path().join("rollout-live.jsonl");
        fs::write(&rollout, "{}").unwrap();
        let metadata = fs::metadata(&rollout).unwrap();

        let data = temp.path().join("fake-lsof-output");
        let payload = format!(
            "p44\0ccodex\0\nf3\0D0x{:x}\0i{}\0n{}\0\n",
            metadata.dev(),
            metadata.ino(),
            rollout.display()
        );
        fs::write(&data, payload.as_bytes()).unwrap();

        let script = temp.path().join("fake-lsof");
        let mut file = fs::File::create(&script).unwrap();
        writeln!(
            file,
            "#!/bin/sh\necho x >> '{}'\ncat '{}'",
            count.display(),
            data.display()
        )
        .unwrap();
        let mut permissions = fs::metadata(&script).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script, permissions).unwrap();

        // `probe_with` swallows the underlying `io::Error` into an opaque
        // `codex_activity_probe_failed` diagnostic (by design, for
        // production redaction). If the assertion below fails, this
        // sidesteps that redaction to surface the real OS error (kind +
        // message) directly, without invoking the script a second time
        // (which would double the `echo x >> count` side effect the
        // subsequent count-lines assertion relies on).
        let (snapshot, diagnostics) = probe_with(script.as_os_str(), SystemTime::UNIX_EPOCH);
        if !diagnostics.is_empty() {
            let direct = Command::new(script.as_os_str()).args(probe_argv()).output();
            panic!(
                "diagnostics: {diagnostics:?}, snapshot.is_empty()={}, lookup={:?}, \
                 direct re-exec result: {:?} (script={}, exists={}, is_file={}, len={:?})",
                snapshot.is_empty(),
                snapshot.lookup(&rollout),
                direct,
                script.display(),
                script.exists(),
                script.is_file(),
                fs::metadata(&script).map(|m| m.len())
            );
        }
        for _ in 0..100 {
            assert!(snapshot.lookup(&rollout).is_some());
        }
        assert_eq!(fs::read_to_string(count).unwrap().lines().count(), 1);
        assert_eq!(
            probe_argv(),
            ["-n", "-P", "-w", "-S", "2", "-F0pcfnDi", "-c", "codex"]
        );
    }
}
