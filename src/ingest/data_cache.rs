use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Path of the cache file for a game: `{dir}/{canonical_id}.xlsx`.
pub fn cache_path(dir: &Path, canonical_id: &str) -> PathBuf {
    dir.join(format!("{canonical_id}.xlsx"))
}

/// Writes a game's raw bytes to the cache atomically (unique temp file +
/// rename), so concurrent writers for the same canonical_id never leave a
/// torn file.
pub fn write_game_data(dir: &Path, canonical_id: &str, bytes: &[u8]) -> anyhow::Result<()> {
    fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(
        ".{canonical_id}.{}.{}.tmp",
        std::process::id(),
        TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let result =
        fs::write(&tmp, bytes).and_then(|()| fs::rename(&tmp, cache_path(dir, canonical_id)));
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result?;
    Ok(())
}

/// Returns the cached bytes for a game, or `None` if missing or unreadable.
/// Callers treat `None` as "download instead"; a corrupt entry self-heals by
/// falling back and re-caching. Files left under stale canonical_ids (parser
/// changes shift hashes) are orphaned, not GC'd — known, accepted cost.
pub fn read_game_data(dir: &Path, canonical_id: &str) -> Option<Vec<u8>> {
    fs::read(cache_path(dir, canonical_id)).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_dir() -> std::path::PathBuf {
        std::env::temp_dir().join(format!("statskboo-cache-test-{}", std::process::id()))
    }

    #[test]
    fn test_cache_path_format() {
        let dir = std::path::Path::new("/tmp/cache");
        let p = cache_path(dir, "b1887910");
        assert_eq!(p, std::path::PathBuf::from("/tmp/cache/b1887910.xlsx"));
    }

    #[test]
    fn test_write_read_roundtrip() {
        let dir = test_dir();
        let cid = "abc12345";
        let bytes = b"PK\x03\x04 fake xlsx bytes";
        write_game_data(&dir, cid, bytes).unwrap();
        assert_eq!(read_game_data(&dir, cid).as_deref(), Some(bytes.as_slice()));
        std::fs::remove_file(cache_path(&dir, cid)).ok();
        std::fs::remove_dir(&dir).ok();
    }

    #[test]
    fn test_read_missing_returns_none() {
        let dir = test_dir();
        assert_eq!(read_game_data(&dir, "missing000"), None);
    }
}
