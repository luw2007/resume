//! Read-only OS process-table acquisition for live-session correlation.
//!
//! The probe executes one platform-specific `ps` command per application
//! invocation: macOS uses numeric `tdev`, while Linux uses its native `tty`
//! name column. All failures degrade to an empty table, so callers retain the
//! positive-evidence-only `Unknown` fallback.

use std::{
    collections::{HashMap, HashSet},
    ffi::{OsStr, OsString},
    io::{self, Read},
    path::Path,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant, SystemTime},
};

/// Maximum wall-clock duration allowed for the process probe.
pub const PROC_PROBE_BUDGET: Duration = Duration::from_millis(300);

pub const DISABLE_PROC_PROBE_ENV: &str = "RESUME_DISABLE_PROC_PROBE";

#[cfg(target_os = "macos")]
const PS_ARGS: [&str; 3] = ["-A", "-o", "pid=,tdev=,etime=,ucomm="];
#[cfg(target_os = "linux")]
const PS_ARGS: [&str; 3] = ["-A", "-o", "pid=,tty=,etime=,comm="];

/// One normalized process-table row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcEntry {
    pub pid: u32,
    pub command: OsString,
    pub tty: Option<OsString>,
    pub elapsed: Option<Duration>,
}

/// Process rows observed at one instant.
#[derive(Clone, Debug, Default)]
pub struct ProcessTable {
    entries: Vec<ProcEntry>,
    observed_at: Option<SystemTime>,
}

impl ProcessTable {
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn observed_at(&self) -> Option<SystemTime> {
        self.observed_at
    }

    pub fn by_command<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a ProcEntry> {
        self.entries
            .iter()
            .filter(move |entry| command_matches(&entry.command, name))
    }

    pub fn ttys_for_command(&self, name: &str) -> Vec<OsString> {
        let mut seen = HashSet::new();
        self.by_command(name)
            .filter_map(|entry| entry.tty.clone())
            .filter(|tty| seen.insert(tty.clone()))
            .collect()
    }

    #[cfg(test)]
    pub(crate) fn from_entries(entries: Vec<ProcEntry>, observed_at: SystemTime) -> Self {
        Self {
            entries,
            observed_at: Some(observed_at),
        }
    }
}

/// Character-device lookup keyed by `(major, minor)`.
#[derive(Clone, Debug, Default)]
pub struct DeviceMap {
    devices: HashMap<(u32, u32), OsString>,
}

impl DeviceMap {
    /// Scan a `/dev`-like root and its `pts` child. Errors yield a partial map.
    pub fn scan(dev_root: &Path) -> Self {
        let mut map = Self::default();
        #[cfg(unix)]
        {
            scan_device_dir(dev_root, &mut map);
            scan_device_dir(&dev_root.join("pts"), &mut map);
        }
        map
    }

    pub fn lookup(&self, major: u32, minor: u32) -> Option<&OsString> {
        self.devices.get(&(major, minor))
    }

    #[cfg(test)]
    fn insert(&mut self, major: u32, minor: u32, name: impl Into<OsString>) {
        self.devices.insert((major, minor), name.into());
    }
}

#[cfg(unix)]
fn scan_device_dir(dir: &Path, map: &mut DeviceMap) {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};

    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if metadata.file_type().is_char_device() {
            let (major, minor) = device_numbers(metadata.rdev());
            map.devices.insert((major, minor), entry.file_name());
        }
    }
}

#[cfg(target_os = "macos")]
fn device_numbers(device: u64) -> (u32, u32) {
    ((device >> 24) as u32, (device & 0x00ff_ffff) as u32)
}

#[cfg(all(unix, not(target_os = "macos")))]
fn device_numbers(device: u64) -> (u32, u32) {
    let major = ((device >> 8) & 0x0fff) | ((device >> 32) & 0xffff_f000);
    let minor = (device & 0x00ff) | ((device >> 12) & 0xffff_ff00);
    (major as u32, minor as u32)
}

fn command_matches(command: &std::ffi::OsStr, name: &str) -> bool {
    command
        .to_string_lossy()
        .split_whitespace()
        // Executable, interpreter script, or `/usr/bin/env <runtime> <script>`.
        // Do not scan arbitrary argv, where a prompt could merely mention OMP.
        .take(3)
        .any(|token| {
            Path::new(token)
                .file_name()
                .is_some_and(|base| base == name)
        })
}

/// Parse captured macOS `ps` output containing numeric `tdev` values.
pub fn parse_ps_output(raw: &str, devices: &DeviceMap, observed_at: SystemTime) -> ProcessTable {
    let entries = raw
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let pid = fields.next()?.parse().ok()?;
            let tdev = fields.next()?;
            let elapsed = fields.next().and_then(parse_elapsed);
            let command = fields.collect::<Vec<_>>().join(" ");
            if command.is_empty() {
                return None;
            }
            let tty = parse_device(tdev)
                .and_then(|(major, minor)| devices.lookup(major, minor))
                .cloned();
            Some(ProcEntry {
                pid,
                command: command.into(),
                tty,
                elapsed,
            })
        })
        .collect();
    ProcessTable {
        entries,
        observed_at: Some(observed_at),
    }
}

#[cfg(any(target_os = "linux", test))]
fn parse_linux_ps_output(raw: &str, observed_at: SystemTime) -> ProcessTable {
    let entries = raw
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let pid = fields.next()?.parse().ok()?;
            let tty = normalize_tty(OsStr::new(fields.next()?));
            let elapsed = fields.next().and_then(parse_elapsed);
            let command = fields.collect::<Vec<_>>().join(" ");
            (!command.is_empty()).then_some(ProcEntry {
                pid,
                command: command.into(),
                tty,
                elapsed,
            })
        })
        .collect();
    ProcessTable {
        entries,
        observed_at: Some(observed_at),
    }
}

#[cfg(any(target_os = "linux", test))]
fn normalize_tty(raw: &OsStr) -> Option<OsString> {
    let tty = raw.to_string_lossy();
    if matches!(tty.as_ref(), "?" | "??" | "-") {
        return None;
    }
    Some(tty.strip_prefix("/dev/").unwrap_or(&tty).into())
}

fn proc_probe_disabled(value: Option<&OsStr>) -> bool {
    value.is_some()
}

fn parse_device(value: &str) -> Option<(u32, u32)> {
    if value == "??" || value == "?" || value == "-" {
        return None;
    }
    let (major, minor) = value.split_once('/').or_else(|| value.split_once(','))?;
    Some((major.parse().ok()?, minor.parse().ok()?))
}

fn parse_elapsed(value: &str) -> Option<Duration> {
    let (days, clock) = match value.split_once('-') {
        Some((days, clock)) => (days.parse::<u64>().ok()?, clock),
        None => (0, value),
    };
    let parts: Vec<u64> = clock
        .split(':')
        .map(str::parse)
        .collect::<Result<_, _>>()
        .ok()?;
    let seconds = match parts.as_slice() {
        [minutes, seconds] => minutes.checked_mul(60)?.checked_add(*seconds)?,
        [hours, minutes, seconds] => hours
            .checked_mul(3600)?
            .checked_add(minutes.checked_mul(60)?)?
            .checked_add(*seconds)?,
        _ => return None,
    };
    Duration::from_secs(days.checked_mul(86_400)?.checked_add(seconds)?).into()
}

/// Acquire a bounded process snapshot. Failure is represented by an empty table.
pub fn snapshot() -> io::Result<ProcessTable> {
    if proc_probe_disabled(std::env::var_os(DISABLE_PROC_PROBE_ENV).as_deref()) {
        return Ok(ProcessTable::empty());
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        return Ok(ProcessTable::empty());
    }
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        let observed_at = SystemTime::now();
        let mut child = match Command::new("ps")
            .args(PS_ARGS)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(child) => child,
            Err(_) => return Ok(ProcessTable::empty()),
        };
        let mut stdout = child.stdout.take().expect("piped stdout");
        let reader = thread::spawn(move || {
            let mut bytes = Vec::new();
            stdout.read_to_end(&mut bytes).map(|_| bytes)
        });
        let deadline = Instant::now() + PROC_PROBE_BUDGET;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(5)),
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break None;
                }
            }
        };
        let Some(_status) = status.filter(|status| status.success()) else {
            let _ = reader.join();
            return Ok(ProcessTable::empty());
        };
        let Ok(Ok(bytes)) = reader.join() else {
            return Ok(ProcessTable::empty());
        };
        let Ok(raw) = String::from_utf8(bytes) else {
            return Ok(ProcessTable::empty());
        };
        #[cfg(target_os = "macos")]
        let table = parse_ps_output(&raw, &DeviceMap::scan(Path::new("/dev")), observed_at);
        #[cfg(target_os = "linux")]
        let table = parse_linux_ps_output(&raw, observed_at);
        Ok(table)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_rows_and_degrades_individual_bad_fields() {
        let mut devices = DeviceMap::default();
        devices.insert(16, 4, "ttys004");
        let observed = SystemTime::UNIX_EPOCH + Duration::from_secs(10);
        let table = parse_ps_output(
            "  42 16/4 01:02 bun /usr/local/bin/omp --resume id  \n 43 ?? 2-03:04:05 omp\n bad row\n 44 99/1 nope other\n",
            &devices,
            observed,
        );
        let entries: Vec<_> = table.by_command("omp").collect();
        assert_eq!(entries.len(), 2);
        assert_eq!(
            entries[0].tty.as_deref(),
            Some(std::ffi::OsStr::new("ttys004"))
        );
        assert_eq!(entries[0].elapsed, Some(Duration::from_secs(62)));
        assert_eq!(entries[1].tty, None);
        assert_eq!(entries[1].elapsed, Some(Duration::from_secs(183_845)));
        assert_eq!(table.observed_at(), Some(observed));
    }

    #[test]
    fn empty_and_unreadable_device_inputs_are_safe() {
        let temp = tempfile::tempdir().unwrap();
        assert!(
            DeviceMap::scan(&temp.path().join("missing"))
                .devices
                .is_empty()
        );
        assert!(parse_ps_output("", &DeviceMap::default(), SystemTime::now()).is_empty());
    }

    #[test]
    fn linux_tty_payload_preserves_named_ttys_without_device_map() {
        let table = parse_linux_ps_output(
            "42 pts/3 00:01 omp\n43 ? 00:02 omp\n",
            SystemTime::UNIX_EPOCH,
        );
        let entries: Vec<_> = table.by_command("omp").collect();
        assert_eq!(
            entries[0].tty.as_deref(),
            Some(std::ffi::OsStr::new("pts/3"))
        );
        assert_eq!(entries[1].tty, None);
    }

    #[test]
    fn process_probe_kill_switch_is_detected() {
        assert!(proc_probe_disabled(Some(std::ffi::OsStr::new("1"))));
        assert!(proc_probe_disabled(Some(std::ffi::OsStr::new(""))));
        assert!(!proc_probe_disabled(None));
    }

    #[test]
    fn ttys_are_distinct_and_command_specific() {
        let table = ProcessTable {
            entries: vec![
                ProcEntry {
                    pid: 1,
                    command: "omp".into(),
                    tty: Some("t1".into()),
                    elapsed: None,
                },
                ProcEntry {
                    pid: 2,
                    command: "omp".into(),
                    tty: Some("t1".into()),
                    elapsed: None,
                },
                ProcEntry {
                    pid: 3,
                    command: "other".into(),
                    tty: Some("t2".into()),
                    elapsed: None,
                },
            ],
            observed_at: Some(SystemTime::now()),
        };
        assert_eq!(table.ttys_for_command("omp"), vec![OsString::from("t1")]);
    }
}
