//! Process-only Preview cache.
//!
//! A bounded in-memory LRU cache for Preview content. No disk cache. Each item
//! is bounded to a maximum size with explicit truncation notice and source
//! path. The total cache is bounded to a maximum byte budget with LRU eviction.

pub mod injection;
pub mod jsonl;
pub mod message;
pub mod snapshot;
pub mod summary;
pub mod text;

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Mutex,
};

/// Maximum total cache size: 64 MiB.
const MAX_CACHE_BYTES: usize = 64 * 1024 * 1024;
/// Maximum size per Preview item: 16 MiB.
const MAX_ITEM_BYTES: usize = 16 * 1024 * 1024;
/// Notice appended when a Preview item is truncated.
const TRUNCATION_NOTICE: &str = "\n[preview truncated due to size limit]";

/// An opaque key identifying a Preview item (integration-owned).
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct PreviewKey {
    pub integration: String,
    pub locator: String,
}

/// One Preview entry, already rendered and terminal-safe.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreviewItem {
    /// Terminal-safe rendered content.
    pub content: String,
    /// The source path the content was derived from, for display.
    pub source_path: PathBuf,
    /// Whether the content was truncated.
    pub truncated: bool,
}

impl PreviewItem {
    /// Build a Preview item from raw content, truncating to the per-item
    /// byte budget and appending a truncation notice if needed.
    pub fn from_content(content: String, source_path: PathBuf) -> Self {
        let (content, truncated) = truncate_item(&content);
        Self {
            content,
            source_path,
            truncated,
        }
    }

    /// Render the full display, including truncation notice and source path.
    pub fn render(&self) -> String {
        let mut out = String::new();
        out.push_str(&self.content);
        if self.truncated {
            out.push_str(TRUNCATION_NOTICE);
        }
        out.push_str("\n[source: ");
        out.push_str(&self.source_path.display().to_string());
        out.push(']');
        out
    }
}

fn truncate_item(content: &str) -> (String, bool) {
    if content.len() <= MAX_ITEM_BYTES {
        return (content.to_string(), false);
    }
    // Truncate at a character boundary within the budget.
    let mut end = MAX_ITEM_BYTES;
    while end > 0 && !content.is_char_boundary(end) {
        end -= 1;
    }
    (content[..end].to_string(), true)
}

/// A process-only LRU Preview cache bounded by total byte budget.
pub struct PreviewCache {
    inner: Mutex<CacheInner>,
}

struct CacheInner {
    entries: HashMap<PreviewKey, (PreviewItem, usize)>,
    /// Access order: front = most recently used, back = least recently used.
    order: Vec<PreviewKey>,
    total_bytes: usize,
    max_bytes: usize,
}

impl Default for PreviewCache {
    fn default() -> Self {
        Self::new(MAX_CACHE_BYTES)
    }
}

impl PreviewCache {
    pub fn new(max_bytes: usize) -> Self {
        Self {
            inner: Mutex::new(CacheInner {
                entries: HashMap::new(),
                order: Vec::new(),
                total_bytes: 0,
                max_bytes,
            }),
        }
    }

    /// Insert or replace a Preview item, evicting LRU entries as needed.
    pub fn put(&self, key: PreviewKey, item: PreviewItem) {
        let mut inner = self.inner.lock().unwrap();
        let item_bytes = item.content.len();

        // If replacing, remove old entry first.
        if let Some((_, old_bytes)) = inner.entries.remove(&key) {
            inner.total_bytes -= old_bytes;
            inner.order.retain(|k| k != &key);
        }

        // Evict LRU entries until the new item fits.
        while inner.total_bytes + item_bytes > inner.max_bytes && !inner.order.is_empty() {
            let lru_key = inner.order.pop().unwrap(); // back = LRU
            if let Some((_, lru_bytes)) = inner.entries.remove(&lru_key) {
                inner.total_bytes -= lru_bytes;
            }
        }

        // Only insert if the item itself fits within the budget.
        if item_bytes <= inner.max_bytes {
            inner.total_bytes += item_bytes;
            inner.order.insert(0, key.clone());
            inner.entries.insert(key, (item, item_bytes));
        }
        // If the item is larger than the entire budget, it is not cached;
        // callers should still return it from a one-shot render.
    }

    /// Look up a Preview item, marking it as recently used.
    pub fn get(&self, key: &PreviewKey) -> Option<PreviewItem> {
        let mut inner = self.inner.lock().unwrap();
        if inner.entries.contains_key(key) {
            // Move to front (MRU).
            inner.order.retain(|k| k != key);
            inner.order.insert(0, key.clone());
            inner.entries.get(key).map(|(item, _)| item.clone())
        } else {
            None
        }
    }

    /// Current number of cached items.
    pub fn len(&self) -> usize {
        self.inner.lock().unwrap().entries.len()
    }

    /// Whether the cache is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Current total bytes used.
    pub fn bytes(&self) -> usize {
        self.inner.lock().unwrap().total_bytes
    }

    /// Clear the cache.
    pub fn clear(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.entries.clear();
        inner.order.clear();
        inner.total_bytes = 0;
    }
}

/// Read preview content from a file path, bounded to the per-item limit.
pub fn read_preview_file(path: &Path) -> std::io::Result<PreviewItem> {
    let metadata = std::fs::metadata(path)?;
    let max_read = MAX_ITEM_BYTES.min(metadata.len() as usize);
    let mut file = std::fs::File::open(path)?;
    let mut buf = vec![0u8; max_read];
    let n = std::io::Read::read(&mut file, &mut buf)?;
    buf.truncate(n);
    let content = String::from_utf8_lossy(&buf).into_owned();
    let truncated = metadata.len() as usize > MAX_ITEM_BYTES;
    // If truncated, we still want the notice; but PreviewItem::from_content
    // handles its own per-item truncation. Since we already bounded the read,
    // set truncated based on file size.
    if truncated {
        // Re-truncate to leave room for nothing extra; from_content won't
        // double-truncate since we already fit within MAX_ITEM_BYTES.
        Ok(PreviewItem {
            content,
            source_path: path.to_path_buf(),
            truncated: true,
        })
    } else {
        Ok(PreviewItem::from_content(content, path.to_path_buf()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(n: &str) -> PreviewKey {
        PreviewKey {
            integration: "test".into(),
            locator: n.into(),
        }
    }

    fn item(text: &str) -> PreviewItem {
        PreviewItem::from_content(text.into(), PathBuf::from("/source"))
    }

    #[test]
    fn put_and_get_round_trip() {
        let cache = PreviewCache::default();
        cache.put(key("1"), item("hello"));
        assert_eq!(cache.get(&key("1")).unwrap().content, "hello");
    }

    #[test]
    fn miss_returns_none() {
        let cache = PreviewCache::default();
        assert!(cache.get(&key("nope")).is_none());
    }

    #[test]
    fn lru_eviction_removes_oldest_when_over_budget() {
        // Small budget to force eviction: each item is 10 bytes.
        let cache = PreviewCache::new(25);
        cache.put(key("a"), item("aaaaaaaaaa")); // 10 bytes
        cache.put(key("b"), item("bbbbbbbbbb")); // 10 bytes, total 20
        cache.put(key("c"), item("cccccccccc")); // 10 bytes, total 30 > 25, evict a
        assert!(cache.get(&key("a")).is_none(), "LRU 'a' should be evicted");
        assert!(cache.get(&key("b")).is_some());
        assert!(cache.get(&key("c")).is_some());
    }

    #[test]
    fn get_marks_as_recently_used() {
        let cache = PreviewCache::new(25);
        cache.put(key("a"), item("aaaaaaaaaa"));
        cache.put(key("b"), item("bbbbbbbbbb"));
        // Access 'a' to make it MRU.
        let _ = cache.get(&key("a"));
        cache.put(key("c"), item("cccccccccc")); // should evict 'b' not 'a'
        assert!(cache.get(&key("a")).is_some());
        assert!(cache.get(&key("b")).is_none());
    }

    #[test]
    fn replace_updates_entry() {
        let cache = PreviewCache::default();
        cache.put(key("1"), item("old"));
        cache.put(key("1"), item("new"));
        assert_eq!(cache.get(&key("1")).unwrap().content, "new");
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn oversized_item_truncated_with_notice() {
        let big = "x".repeat(MAX_ITEM_BYTES + 1000);
        let item = PreviewItem::from_content(big, PathBuf::from("/big"));
        assert!(item.truncated);
        assert!(item.content.len() <= MAX_ITEM_BYTES);
        let rendered = item.render();
        assert!(rendered.contains("[preview truncated due to size limit]"));
        assert!(rendered.contains("/big"));
    }

    #[test]
    fn item_larger_than_total_budget_not_cached() {
        let cache = PreviewCache::new(100);
        let huge = "y".repeat(200);
        cache.put(
            key("huge"),
            PreviewItem::from_content(huge.clone(), PathBuf::from("/h")),
        );
        // The item is too large to fit; it's not stored.
        assert!(cache.get(&key("huge")).is_none());
    }

    #[test]
    fn render_includes_source_path() {
        let item = PreviewItem::from_content("content".into(), PathBuf::from("/path/to/source"));
        let rendered = item.render();
        assert!(rendered.contains("content"));
        assert!(rendered.contains("/path/to/source"));
    }

    #[test]
    fn read_preview_file_from_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("preview.txt");
        std::fs::write(&path, "preview content").unwrap();
        let item = read_preview_file(&path).unwrap();
        assert_eq!(item.content, "preview content");
        assert!(!item.truncated);
        assert_eq!(item.source_path, path);
    }

    #[test]
    fn clear_empties_cache() {
        let cache = PreviewCache::default();
        cache.put(key("1"), item("x"));
        cache.clear();
        assert!(cache.is_empty());
        assert_eq!(cache.bytes(), 0);
    }
}
