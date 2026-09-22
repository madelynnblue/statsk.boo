//! On-disk raw-bytes cache for statsbook downloads.
//!
//! The cache is keyed by the WFTDA-first identity, `statsbooks.drive_file_id`,
//! and stores exactly the bytes `DriveClient::download_file` returns at
//! `{dir}/{drive_file_id}.xlsx`. It exists so that the expensive, rate-limited
//! part of stage 4 (the Drive download) is paid once per Drive revision: a
//! `PARSER_VERSION` bump re-parses from the cache instead of re-downloading the
//! whole repository.
//!
//! Freshness rule: cached bytes are used when the cache file exists and its
//! mtime is at or after the row's `modified_time` (Drive's exact
//! `modifiedTime`); a NULL `modified_time` always trusts the cache. Otherwise
//! the cache is stale and the file must be re-downloaded and the cache
//! overwritten. Because a `PARSER_VERSION` bump does not change `modified_time`,
//! it hits the cache; a re-upload changes `modified_time` and misses it.
//!
//! Writes are atomic (unique temp file in the same directory, then rename), so
//! a crash or concurrent writer never leaves a torn `.xlsx`.
//!
//! Judgement calls:
//! - The cache directory is optional. No directory configured means no cache
//!   reads and no writes — the pre-cache always-download behaviour.
//! - A missing directory is not an error: `read_cached`/`decide_cache` report a
//!   miss and `write_cached` creates the directory on first write.
//! - Unreadable/corrupt entries are treated as a miss and self-heal when the
//!   file is re-downloaded; cache writes are best-effort and never fail a parse.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Whether `drive_file_id` may be used as a single filename component.
///
/// Drive ids are opaque strings, but they flow into a filesystem path, so a
/// separator, `.`/`..`, or NUL must never be accepted. Anything else is left to
/// the filesystem.
pub fn is_safe_file_id(drive_file_id: &str) -> bool {
    !drive_file_id.is_empty()
        && drive_file_id != "."
        && drive_file_id != ".."
        && !drive_file_id.contains('\0')
        && !drive_file_id.contains('/')
        && !drive_file_id.contains('\\')
}

/// Cache path for a statsbook: `{dir}/{drive_file_id}.xlsx`, or `None` when the
/// id is not a safe filename component.
pub fn cache_path(dir: &Path, drive_file_id: &str) -> Option<PathBuf> {
    if !is_safe_file_id(drive_file_id) {
        return None;
    }
    Some(dir.join(format!("{drive_file_id}.xlsx")))
}

/// Writes `bytes` to the cache atomically (unique temp file + rename).
///
/// Creates `dir` if needed. The temp file is removed on failure so a failed
/// write leaves no debris.
pub fn write_cached(dir: &Path, drive_file_id: &str, bytes: &[u8]) -> Result<()> {
    let dest = cache_path(dir, drive_file_id).with_context(|| {
        format!("refusing to cache under unsafe drive_file_id {drive_file_id:?}")
    })?;
    fs::create_dir_all(dir)
        .with_context(|| format!("creating cache directory {}", dir.display()))?;

    let tmp = dir.join(format!(
        ".{drive_file_id}.{}.{}.tmp",
        std::process::id(),
        TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let result = fs::write(&tmp, bytes).and_then(|()| fs::rename(&tmp, &dest));
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result.with_context(|| format!("writing cache file {}", dest.display()))?;
    Ok(())
}

/// Returns the cached bytes, or `None` when the id is unsafe, the file is
/// missing, or it cannot be read. Callers treat `None` as "download instead".
pub fn read_cached(dir: &Path, drive_file_id: &str) -> Option<Vec<u8>> {
    fs::read(cache_path(dir, drive_file_id)?).ok()
}

/// Removes a cache file, best-effort: a missing file is not an error.
pub fn remove_cached(dir: &Path, drive_file_id: &str) {
    let Some(path) = cache_path(dir, drive_file_id) else {
        return;
    };
    match fs::remove_file(&path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => tracing::warn!("failed to remove cache file {}: {e}", path.display()),
    }
}

/// What the cache holds for one statsbook.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheDecision {
    /// No cache directory is configured: always download.
    Disabled,
    /// No usable cache file: download and populate it.
    Miss,
    /// A cache file exists but predates Drive's `modified_time`: download and
    /// overwrite it.
    Stale,
    /// A fresh cache file exists: parse its bytes without downloading.
    Use,
}

/// The freshness comparison, factored out so it is testable with explicit
/// timestamps and no filesystem: cached bytes are fresh when the cache file's
/// mtime is at or after Drive's `modified_time`. A NULL `modified_time` always
/// trusts the cache.
pub fn cache_is_fresh(cache_mtime: DateTime<Utc>, modified_time: Option<DateTime<Utc>>) -> bool {
    match modified_time {
        None => true,
        Some(modified_time) => cache_mtime >= modified_time,
    }
}

/// Decides whether `drive_file_id`'s cached bytes may be parsed. Stats the
/// cache file; the mtime comparison itself is [`cache_is_fresh`].
pub fn decide_cache(
    dir: Option<&Path>,
    drive_file_id: &str,
    modified_time: Option<DateTime<Utc>>,
) -> CacheDecision {
    let Some(dir) = dir else {
        return CacheDecision::Disabled;
    };
    let Some(path) = cache_path(dir, drive_file_id) else {
        return CacheDecision::Miss;
    };
    let Ok(metadata) = fs::metadata(&path) else {
        return CacheDecision::Miss;
    };
    let Ok(mtime) = metadata.modified() else {
        return CacheDecision::Miss;
    };
    if cache_is_fresh(mtime.into(), modified_time) {
        CacheDecision::Use
    } else {
        CacheDecision::Stale
    }
}

/// Convenience for stage 4: the cached bytes when the freshness rule says they
/// are usable, else `None`.
pub fn read_cached_if_fresh(
    dir: Option<&Path>,
    drive_file_id: &str,
    modified_time: Option<DateTime<Utc>>,
) -> Option<Vec<u8>> {
    if decide_cache(dir, drive_file_id, modified_time) != CacheDecision::Use {
        return None;
    }
    read_cached(dir?, drive_file_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime};

    /// A unique temp directory per test, removed by `Drop`.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "statskboo-cache-test-{}-{}-{}",
                tag,
                std::process::id(),
                TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&path).unwrap();
            TempDir(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn utc(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> DateTime<Utc> {
        use chrono::TimeZone;
        Utc.with_ymd_and_hms(y, mo, d, h, mi, s).unwrap()
    }

    #[test]
    fn cache_path_uses_drive_file_id_and_xlsx() {
        let dir = Path::new("/tmp/cache");
        assert_eq!(
            cache_path(dir, "1AbC_d-ef").unwrap(),
            PathBuf::from("/tmp/cache/1AbC_d-ef.xlsx")
        );
    }

    #[test]
    fn guard_rejects_unsafe_ids() {
        assert!(is_safe_file_id("1AbC_d-ef.xlsx"));
        assert!(is_safe_file_id("abc12345"));
        assert!(!is_safe_file_id(""));
        assert!(!is_safe_file_id("."));
        assert!(!is_safe_file_id(".."));
        assert!(!is_safe_file_id("../escape"));
        assert!(!is_safe_file_id("a/b"));
        assert!(!is_safe_file_id("/abs"));
        assert!(!is_safe_file_id("a\\b"));
        assert!(!is_safe_file_id("a\0b"));

        assert!(cache_path(Path::new("/tmp/cache"), "../escape").is_none());
    }

    #[test]
    fn write_read_remove_roundtrip() {
        let dir = TempDir::new("roundtrip");
        let bytes = b"PK\x03\x04 fake xlsx bytes";
        write_cached(dir.path(), "abc12345", bytes).unwrap();
        assert_eq!(
            read_cached(dir.path(), "abc12345").as_deref(),
            Some(bytes.as_slice())
        );

        remove_cached(dir.path(), "abc12345");
        assert_eq!(read_cached(dir.path(), "abc12345"), None);
        // Removing an already-missing entry is a no-op, not an error.
        remove_cached(dir.path(), "abc12345");
    }

    #[test]
    fn write_is_atomic_and_leaves_no_temp_files() {
        let dir = TempDir::new("atomic");
        write_cached(dir.path(), "again", b"one").unwrap();
        write_cached(dir.path(), "again", b"two").unwrap();
        assert_eq!(
            read_cached(dir.path(), "again").as_deref(),
            Some(&b"two"[..])
        );
        let leftovers: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".tmp"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "temp files left behind: {leftovers:?}"
        );
    }

    #[test]
    fn write_creates_missing_directory() {
        let parent = TempDir::new("mkdir");
        let nested = parent.path().join("does/not/exist/yet");
        write_cached(&nested, "abc", b"data").unwrap();
        assert_eq!(read_cached(&nested, "abc").as_deref(), Some(&b"data"[..]));
    }

    #[test]
    fn write_rejects_unsafe_id() {
        let dir = TempDir::new("unsafe-write");
        assert!(write_cached(dir.path(), "../escape", b"x").is_err());
        assert!(read_cached(dir.path(), "../escape").is_none());
    }

    #[test]
    fn read_missing_returns_none() {
        let dir = TempDir::new("missing");
        assert_eq!(read_cached(dir.path(), "missing000"), None);
    }

    #[test]
    fn freshness_rule_at_boundaries() {
        let modified = utc(2026, 5, 2, 12, 0, 0);

        // mtime after Drive's modified_time: fresh.
        assert!(cache_is_fresh(
            modified + Duration::from_secs(1),
            Some(modified)
        ));
        // mtime exactly equal: fresh (>=, so a PARSER_VERSION bump hits).
        assert!(cache_is_fresh(modified, Some(modified)));
        // mtime before: stale.
        assert!(!cache_is_fresh(
            modified - Duration::from_secs(1),
            Some(modified)
        ));
        // NULL modified_time always trusts the cache.
        assert!(cache_is_fresh(modified - Duration::from_secs(3600), None));
    }

    #[test]
    fn decide_cache_covers_all_four_states() {
        let dir = TempDir::new("decide");
        let modified = utc(2026, 5, 2, 12, 0, 0);
        let path = cache_path(dir.path(), "row1").unwrap();

        // No directory configured.
        assert_eq!(
            decide_cache(None, "row1", Some(modified)),
            CacheDecision::Disabled
        );

        // No file yet.
        assert_eq!(
            decide_cache(Some(dir.path()), "row1", Some(modified)),
            CacheDecision::Miss
        );

        // Write the file, then force its mtime to exactly Drive's time.
        write_cached(dir.path(), "row1", b"bytes").unwrap();
        let file = fs::File::options().write(true).open(&path).unwrap();
        file.set_modified(SystemTime::from(modified)).unwrap();
        drop(file);

        assert_eq!(
            decide_cache(Some(dir.path()), "row1", Some(modified)),
            CacheDecision::Use,
            "equal mtime must be a hit"
        );
        assert_eq!(
            read_cached_if_fresh(Some(dir.path()), "row1", Some(modified)).as_deref(),
            Some(&b"bytes"[..])
        );

        // A newer Drive revision makes the same file stale.
        let newer = modified + Duration::from_secs(60);
        assert_eq!(
            decide_cache(Some(dir.path()), "row1", Some(newer)),
            CacheDecision::Stale
        );
        assert_eq!(
            read_cached_if_fresh(Some(dir.path()), "row1", Some(newer)),
            None
        );

        // NULL modified_time trusts the stale-looking file.
        assert_eq!(
            decide_cache(Some(dir.path()), "row1", None),
            CacheDecision::Use
        );

        // Unsafe ids are always a miss, even with a configured dir.
        assert_eq!(
            decide_cache(Some(dir.path()), "../row1", None),
            CacheDecision::Miss
        );
    }
}
