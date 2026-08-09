//! Read-only regression helpers that snapshot bytes, mtimes, and directory
//! entries around discovery/Preview.
//!
//! These helpers are used in tests to assert that discovery and Preview never
//! modify agent files, directories, mtimes, or directory entries. The
//! invariant: the snapshot before and after must be identical.

use std::{
    collections::BTreeMap,
    fs,
    io::Read,
    path::{Path, PathBuf},
    time::SystemTime,
};

/// A snapshot of a directory tree: file paths (relative), sizes, mtimes, and
/// a content hash. Also captures directory entry names and order (sorted).
#[derive(Clone, Debug)]
pub struct DirSnapshot {
    pub root: PathBuf,
    pub entries: BTreeMap<PathBuf, EntrySnapshot>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EntrySnapshot {
    pub kind: EntryKind,
    pub size: u64,
    pub mtime: SystemTime,
    pub sha256: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EntryKind {
    File,
    Dir,
    Symlink,
}

/// Take a read-only snapshot of a directory tree (non-recursive into the root
/// is supported via `recursive` flag).
pub fn snapshot_dir(root: &Path, recursive: bool) -> std::io::Result<DirSnapshot> {
    let mut entries = BTreeMap::new();
    snapshot_into(root, root, recursive, &mut entries)?;
    Ok(DirSnapshot {
        root: root.to_path_buf(),
        entries,
    })
}

fn snapshot_into(
    root: &Path,
    current: &Path,
    recursive: bool,
    entries: &mut BTreeMap<PathBuf, EntrySnapshot>,
) -> std::io::Result<()> {
    for entry in fs::read_dir(current)? {
        let entry = entry?;
        let path = entry.path();
        let rel = path.strip_prefix(root).unwrap_or(&path).to_path_buf();
        let metadata = entry.metadata()?;
        let file_type = entry.file_type()?;
        let kind = if file_type.is_symlink() {
            EntryKind::Symlink
        } else if file_type.is_dir() {
            EntryKind::Dir
        } else {
            EntryKind::File
        };
        let sha256 = if kind == EntryKind::File {
            Some(sha256_file(&path)?)
        } else {
            None
        };
        entries.insert(
            rel,
            EntrySnapshot {
                kind,
                size: metadata.len(),
                mtime: metadata.modified()?,
                sha256,
            },
        );
        if recursive && kind == EntryKind::Dir {
            snapshot_into(root, &path, recursive, entries)?;
        }
    }
    Ok(())
}

/// Snapshot a single file's bytes and mtime.
pub fn snapshot_file(path: &Path) -> std::io::Result<FileSnapshot> {
    let metadata = fs::metadata(path)?;
    let mut file = fs::File::open(path)?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf)?;
    Ok(FileSnapshot {
        bytes: buf,
        mtime: metadata.modified()?,
        size: metadata.len(),
    })
}

#[derive(Clone, Debug)]
pub struct FileSnapshot {
    pub bytes: Vec<u8>,
    pub mtime: SystemTime,
    pub size: u64,
}

/// Compare two directory snapshots and return a list of differences.
pub fn diff_snapshots(before: &DirSnapshot, after: &DirSnapshot) -> Vec<SnapshotDiff> {
    let mut diffs = Vec::new();
    for (path, before_entry) in &before.entries {
        match after.entries.get(path) {
            None => diffs.push(SnapshotDiff::Removed(path.clone())),
            Some(after_entry) => {
                if before_entry != after_entry {
                    diffs.push(SnapshotDiff::Changed {
                        path: path.clone(),
                        before: before_entry.clone(),
                        after: after_entry.clone(),
                    });
                }
            }
        }
    }
    for path in after.entries.keys() {
        if !before.entries.contains_key(path) {
            diffs.push(SnapshotDiff::Added(path.clone()));
        }
    }
    diffs
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SnapshotDiff {
    Added(PathBuf),
    Removed(PathBuf),
    Changed {
        path: PathBuf,
        before: EntrySnapshot,
        after: EntrySnapshot,
    },
}

/// Assert that a directory snapshot is unchanged (for test assertions).
pub fn assert_unchanged(before: &DirSnapshot, after: &DirSnapshot) {
    let diffs = diff_snapshots(before, after);
    if !diffs.is_empty() {
        panic!(
            "directory snapshot changed (discovery/Preview must be read-only):\n{:#?}",
            diffs
        );
    }
}

/// Assert that a file's bytes and mtime are unchanged.
pub fn assert_file_unchanged(before: &FileSnapshot, after: &FileSnapshot) {
    assert_eq!(
        before.bytes, after.bytes,
        "file bytes changed (discovery/Preview must be read-only)"
    );
    assert_eq!(
        before.mtime, after.mtime,
        "file mtime changed (discovery/Preview must be read-only)"
    );
}

// Minimal SHA-256 implementation for content hashing in tests.
// This avoids adding a crypto dependency just for test snapshots.
fn sha256_file(path: &Path) -> std::io::Result<String> {
    let mut file = fs::File::open(path)?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf)?;
    Ok(sha256_bytes(&buf))
}

pub fn sha256_bytes(input: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input);
    hasher.finalize()
}

// Minimal SHA-256 (FIPS 180-4). Only for test snapshot integrity, not security.
struct Sha256 {
    h: [u32; 8],
    buffer: Vec<u8>,
    total_len: u64,
}

impl Sha256 {
    fn new() -> Self {
        Self {
            h: [
                0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
                0x5be0cd19,
            ],
            buffer: Vec::new(),
            total_len: 0,
        }
    }

    fn update(&mut self, data: &[u8]) {
        self.total_len += data.len() as u64;
        self.buffer.extend_from_slice(data);
        while self.buffer.len() >= 64 {
            let block: [u8; 64] = self.buffer[..64].try_into().unwrap();
            self.process_block(&block);
            self.buffer.drain(..64);
        }
    }

    fn finalize(mut self) -> String {
        let bit_len = self.total_len.wrapping_mul(8);
        self.buffer.push(0x80);
        while self.buffer.len() % 64 != 56 {
            self.buffer.push(0);
        }
        self.buffer.extend_from_slice(&bit_len.to_be_bytes());
        while self.buffer.len() >= 64 {
            let block: [u8; 64] = self.buffer[..64].try_into().unwrap();
            self.process_block(&block);
            self.buffer.drain(..64);
        }
        self.h
            .iter()
            .map(|w| format!("{:08x}", w))
            .collect::<String>()
    }

    fn process_block(&mut self, block: &[u8; 64]) {
        const K: [u32; 64] = [
            0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
            0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
            0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
            0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
            0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
            0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
            0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
            0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
            0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
            0xc67178f2,
        ];
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                block[i * 4],
                block[i * 4 + 1],
                block[i * 4 + 2],
                block[i * 4 + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let mut a = self.h[0];
        let mut b = self.h[1];
        let mut c = self.h[2];
        let mut d = self.h[3];
        let mut e = self.h[4];
        let mut f = self.h[5];
        let mut g = self.h[6];
        let mut h = self.h[7];
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let temp1 = h
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }
        self.h[0] = self.h[0].wrapping_add(a);
        self.h[1] = self.h[1].wrapping_add(b);
        self.h[2] = self.h[2].wrapping_add(c);
        self.h[3] = self.h[3].wrapping_add(d);
        self.h[4] = self.h[4].wrapping_add(e);
        self.h[5] = self.h[5].wrapping_add(f);
        self.h[6] = self.h[6].wrapping_add(g);
        self.h[7] = self.h[7].wrapping_add(h);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_known_vectors() {
        assert_eq!(
            sha256_bytes(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_bytes(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn snapshot_captures_files_and_dirs() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "hello").unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("sub/b.txt"), "world").unwrap();

        let snap = snapshot_dir(dir.path(), true).unwrap();
        assert!(snap.entries.contains_key(Path::new("a.txt")));
        assert!(snap.entries.contains_key(Path::new("sub")));
        assert!(snap.entries.contains_key(Path::new("sub/b.txt")));
    }

    #[test]
    fn diff_detects_added_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "hello").unwrap();
        let before = snapshot_dir(dir.path(), true).unwrap();
        std::fs::write(dir.path().join("b.txt"), "new").unwrap();
        let after = snapshot_dir(dir.path(), true).unwrap();
        let diffs = diff_snapshots(&before, &after);
        assert!(
            diffs
                .iter()
                .any(|d| matches!(d, SnapshotDiff::Added(p) if p == Path::new("b.txt")))
        );
    }

    #[test]
    fn diff_detects_changed_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "hello").unwrap();
        let before = snapshot_dir(dir.path(), true).unwrap();
        std::fs::write(dir.path().join("a.txt"), "changed").unwrap();
        let after = snapshot_dir(dir.path(), true).unwrap();
        let diffs = diff_snapshots(&before, &after);
        assert!(diffs.iter().any(
            |d| matches!(d, SnapshotDiff::Changed { path, .. } if path == Path::new("a.txt"))
        ));
    }

    #[test]
    fn diff_detects_removed_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "hello").unwrap();
        let before = snapshot_dir(dir.path(), true).unwrap();
        std::fs::remove_file(dir.path().join("a.txt")).unwrap();
        let after = snapshot_dir(dir.path(), true).unwrap();
        let diffs = diff_snapshots(&before, &after);
        assert!(
            diffs
                .iter()
                .any(|d| matches!(d, SnapshotDiff::Removed(p) if p == Path::new("a.txt")))
        );
    }

    #[test]
    fn assert_unchanged_passes_on_read_only_access() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "hello").unwrap();
        let before = snapshot_dir(dir.path(), true).unwrap();
        // Simulate read-only access: just read.
        let _ = std::fs::read(dir.path().join("a.txt")).unwrap();
        let after = snapshot_dir(dir.path(), true).unwrap();
        assert_unchanged(&before, &after);
    }

    #[test]
    fn file_snapshot_captures_bytes_and_mtime() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.jsonl");
        std::fs::write(&path, b"{\"a\":1}\n").unwrap();
        let snap = snapshot_file(&path).unwrap();
        assert_eq!(snap.bytes, b"{\"a\":1}\n");
        assert_eq!(snap.size, 8);
    }

    #[test]
    fn read_only_access_does_not_change_file_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.jsonl");
        std::fs::write(&path, b"{\"a\":1}\n").unwrap();
        let before = snapshot_file(&path).unwrap();
        // Read-only access.
        let _ = std::fs::read(&path).unwrap();
        let after = snapshot_file(&path).unwrap();
        assert_file_unchanged(&before, &after);
    }

    #[test]
    fn snapshot_captures_symlinks() {
        #[cfg(unix)]
        {
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(dir.path().join("real.txt"), "data").unwrap();
            std::os::unix::fs::symlink("real.txt", dir.path().join("link.txt")).unwrap();
            let snap = snapshot_dir(dir.path(), false).unwrap();
            let link = snap.entries.get(Path::new("link.txt")).unwrap();
            assert_eq!(link.kind, EntryKind::Symlink);
        }
    }
}
