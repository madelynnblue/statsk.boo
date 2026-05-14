use axum::http::HeaderMap;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::RwLock;

pub struct CachedEntry {
    pub headers: HeaderMap,
    pub body: Vec<u8>, // zstd-compressed
}

struct CacheInner {
    entries: HashMap<String, Arc<CachedEntry>>,
    used_bytes: usize,
}

pub struct Cache {
    inner: RwLock<CacheInner>,
    max_bytes: usize,
}

impl Cache {
    pub fn new() -> Self {
        let total = available_memory_bytes();
        let max_bytes = (total as f64 * CACHE_FILL_FRACTION) as usize;
        tracing::info!(
            "cache: max size {} MiB ({:.0}% of {} MiB)",
            max_bytes / 1024 / 1024,
            CACHE_FILL_FRACTION * 100.0,
            total / 1024 / 1024,
        );
        Self {
            inner: RwLock::new(CacheInner {
                entries: HashMap::new(),
                used_bytes: 0,
            }),
            max_bytes,
        }
    }

    pub fn get(&self, key: &str) -> Option<Arc<CachedEntry>> {
        self.inner
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .entries
            .get(key)
            .cloned()
    }

    pub fn set(&self, key: String, entry: Arc<CachedEntry>) {
        let entry_size = entry.body.len();
        let mut inner = self.inner.write().unwrap_or_else(|p| p.into_inner());
        let old_size = inner.entries.get(&key).map(|e| e.body.len()).unwrap_or(0);
        let new_used = inner.used_bytes.saturating_sub(old_size) + entry_size;
        if new_used > self.max_bytes {
            return;
        }
        inner.used_bytes = new_used;
        inner.entries.insert(key, entry);
    }

    pub fn clear(&self) {
        let mut inner = self.inner.write().unwrap_or_else(|p| p.into_inner());
        inner.entries.clear();
        inner.used_bytes = 0;
    }
}

impl Default for Cache {
    fn default() -> Self {
        Self::new()
    }
}

const CACHE_FILL_FRACTION: f64 = 0.75;

fn available_memory_bytes() -> usize {
    // Try cgroup v2 memory limit (set on Cloud Run and other container runtimes)
    if let Ok(s) = std::fs::read_to_string("/sys/fs/cgroup/memory.max") {
        let trimmed = s.trim();
        if trimmed != "max" {
            if let Ok(n) = trimmed.parse::<usize>() {
                return n;
            }
        }
    }
    // Fall back to total physical memory
    if let Ok(s) = std::fs::read_to_string("/proc/meminfo") {
        for line in s.lines() {
            if let Some(rest) = line.strip_prefix("MemTotal:") {
                if let Ok(kb) = rest
                    .split_whitespace()
                    .next()
                    .unwrap_or("0")
                    .parse::<usize>()
                {
                    return kb * 1024;
                }
            }
        }
    }
    512 * 1024 * 1024 // 512 MiB fallback
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_on_empty_returns_none() {
        let c = Cache::new();
        assert!(c.get("/foo").is_none());
    }

    #[test]
    fn set_then_get_returns_entry() {
        let c = Cache::new();
        let entry = Arc::new(CachedEntry {
            headers: HeaderMap::new(),
            body: b"compressed".to_vec(),
        });
        c.set("/foo".to_string(), entry);
        let got = c.get("/foo").unwrap();
        assert_eq!(got.body, b"compressed");
    }

    #[test]
    fn clear_removes_all_entries() {
        let c = Cache::new();
        c.set(
            "/a".to_string(),
            Arc::new(CachedEntry {
                headers: HeaderMap::new(),
                body: vec![1],
            }),
        );
        c.set(
            "/b".to_string(),
            Arc::new(CachedEntry {
                headers: HeaderMap::new(),
                body: vec![2],
            }),
        );
        c.clear();
        assert!(c.get("/a").is_none());
        assert!(c.get("/b").is_none());
    }

    #[test]
    fn set_overwrites_existing_key() {
        let c = Cache::new();
        c.set(
            "/x".to_string(),
            Arc::new(CachedEntry {
                headers: HeaderMap::new(),
                body: b"first".to_vec(),
            }),
        );
        c.set(
            "/x".to_string(),
            Arc::new(CachedEntry {
                headers: HeaderMap::new(),
                body: b"second".to_vec(),
            }),
        );
        assert_eq!(c.get("/x").unwrap().body, b"second");
    }

    #[test]
    fn set_respects_memory_limit() {
        // Build a cache with a tiny max_bytes (10 bytes)
        let c = Cache {
            inner: RwLock::new(CacheInner {
                entries: HashMap::new(),
                used_bytes: 0,
            }),
            max_bytes: 10,
        };
        // 5 bytes fits
        c.set(
            "/small".to_string(),
            Arc::new(CachedEntry {
                headers: HeaderMap::new(),
                body: vec![0u8; 5],
            }),
        );
        assert!(c.get("/small").is_some());
        // 20 bytes exceeds remaining capacity (5 used + 20 > 10)
        c.set(
            "/big".to_string(),
            Arc::new(CachedEntry {
                headers: HeaderMap::new(),
                body: vec![0u8; 20],
            }),
        );
        assert!(
            c.get("/big").is_none(),
            "entry exceeding memory limit must be rejected"
        );
    }

    #[test]
    fn clear_resets_used_bytes() {
        let c = Cache {
            inner: RwLock::new(CacheInner {
                entries: HashMap::new(),
                used_bytes: 0,
            }),
            max_bytes: 10,
        };
        c.set(
            "/a".to_string(),
            Arc::new(CachedEntry {
                headers: HeaderMap::new(),
                body: vec![0u8; 8],
            }),
        );
        assert!(c.get("/a").is_some());
        c.clear();
        // After clear, used_bytes should be 0, so /b (8 bytes) should fit again
        c.set(
            "/b".to_string(),
            Arc::new(CachedEntry {
                headers: HeaderMap::new(),
                body: vec![0u8; 8],
            }),
        );
        assert!(
            c.get("/b").is_some(),
            "after clear, memory limit should be reset"
        );
    }
}
