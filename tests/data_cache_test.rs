//! No-Drive verification for the on-disk statsbook byte cache.
//!
//! `DriveClient` cannot be exercised without a Google service account, so this
//! test drives the cache decision directly against a live migrated
//! `statskboo_v2` plus real files in a temp directory. It proves:
//!
//! - a cache file whose mtime equals the row's `modified_time` is a hit (the
//!   `PARSER_VERSION`-bump case: `modified_time` is unchanged);
//! - a newer `modified_time` makes the same file stale (re-upload case);
//! - a NULL `modified_time` trusts the cache;
//! - `statsbook_index::mark_removed` deletes the cache file with the row.
//!
//! Ignored by default (needs the migrated scratch database). Run with:
//!
//! ```text
//! DATABASE_URL=postgresql://root@cockroachdb:26257/statskboo_v2?sslmode=disable \
//!   SQLX_OFFLINE=false cargo test --test data_cache_test -- --ignored --nocapture
//! ```
//!
//! The sentinel ids are prefixed `__data_cache_test__` and are deleted at the
//! start and end. The removal check uses an ancient `last_seen` and marker so
//! `mark_removed` can never touch another test's rows.

use chrono::{DateTime, TimeZone, Utc};
use sqlx::PgPool;
use statskboo::ingest::data_cache::{
    CacheDecision, cache_path, decide_cache, read_cached_if_fresh,
};
use statskboo::ingest::statsbook_index::mark_removed;

const ID_FRESH: &str = "__data_cache_test__fresh";
const ID_REMOVED: &str = "__data_cache_test__removed";
const ALL_IDS: [&str; 2] = [ID_FRESH, ID_REMOVED];

fn db_url() -> String {
    std::env::var("DATABASE_URL").unwrap_or_else(|_| {
        "postgresql://root@cockroachdb:26257/statskboo_v2?sslmode=disable".into()
    })
}

struct TempDir(std::path::PathBuf);

impl TempDir {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("statskboo-data-cache-test-{}", std::process::id()));
        std::fs::create_dir_all(&path).unwrap();
        TempDir(path)
    }

    fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

async fn cleanup(pool: &PgPool) {
    for id in ALL_IDS {
        sqlx::query("DELETE FROM statsbooks WHERE drive_file_id = $1")
            .bind(id)
            .execute(pool)
            .await
            .unwrap();
    }
}

async fn insert_statsbook(pool: &PgPool, id: &str, modified: Option<DateTime<Utc>>) {
    sqlx::query(
        r#"INSERT INTO statsbooks
             (drive_file_id, name, quarter, mime_type, modified_time,
              first_seen, last_seen, parse_status, parser_version, parse_error)
           VALUES ($1, $2, 'test', NULL, $3, now(), now(), 'pending', NULL, NULL)
           ON CONFLICT (drive_file_id) DO UPDATE SET
             modified_time  = EXCLUDED.modified_time,
             parse_status   = 'pending',
             parser_version = NULL,
             parse_error    = NULL"#,
    )
    .bind(id)
    .bind(format!("{id}.xlsx"))
    .bind(modified)
    .execute(pool)
    .await
    .unwrap();
}

async fn set_modified_time(pool: &PgPool, id: &str, modified: Option<DateTime<Utc>>) {
    sqlx::query("UPDATE statsbooks SET modified_time = $2 WHERE drive_file_id = $1")
        .bind(id)
        .bind(modified)
        .execute(pool)
        .await
        .unwrap();
}

async fn row_exists(pool: &PgPool, id: &str) -> bool {
    sqlx::query_scalar::<_, i64>("SELECT count(*)::INT8 FROM statsbooks WHERE drive_file_id = $1")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
        > 0
}

/// Writes a cache entry and forces its mtime to exactly `mtime`.
fn seed_cache(dir: &std::path::Path, id: &str, bytes: &[u8], mtime: DateTime<Utc>) {
    let path = cache_path(dir, id).unwrap();
    std::fs::write(&path, bytes).unwrap();
    let file = std::fs::File::options().write(true).open(&path).unwrap();
    file.set_modified(std::time::SystemTime::from(mtime))
        .unwrap();
}

#[tokio::test]
#[ignore = "requires a live migrated statskboo_v2 database; run with --ignored"]
async fn cache_decision_and_removal_without_drive() {
    let pool = PgPool::connect(&db_url()).await.unwrap();
    cleanup(&pool).await;

    let dir = TempDir::new();
    let base = Utc.with_ymd_and_hms(2026, 5, 2, 12, 0, 0).unwrap();
    let bytes: &[u8] = b"PK\x03\x04 cached xlsx bytes";

    // ---- 1. Equal mtime is a hit (the PARSER_VERSION-bump case) ------------
    insert_statsbook(&pool, ID_FRESH, Some(base)).await;
    seed_cache(dir.path(), ID_FRESH, bytes, base);

    // Read the stored row back so the decision uses the DB's value, not ours.
    let modified: Option<DateTime<Utc>> =
        sqlx::query_scalar("SELECT modified_time FROM statsbooks WHERE drive_file_id = $1")
            .bind(ID_FRESH)
            .fetch_one(&pool)
            .await
            .unwrap();
    let decision = decide_cache(Some(dir.path()), ID_FRESH, modified);
    let read = read_cached_if_fresh(Some(dir.path()), ID_FRESH, modified);
    println!(
        "equal modified_time: decision={decision:?} bytes={:?} row_modified={modified:?}",
        read.as_deref()
    );
    assert_eq!(decision, CacheDecision::Use);
    assert_eq!(read.as_deref(), Some(bytes));

    // ---- 2. A newer Drive revision makes the same file stale ---------------
    let newer = base + chrono::Duration::seconds(60);
    set_modified_time(&pool, ID_FRESH, Some(newer)).await;
    let modified: Option<DateTime<Utc>> =
        sqlx::query_scalar("SELECT modified_time FROM statsbooks WHERE drive_file_id = $1")
            .bind(ID_FRESH)
            .fetch_one(&pool)
            .await
            .unwrap();
    let decision = decide_cache(Some(dir.path()), ID_FRESH, modified);
    println!("newer modified_time: decision={decision:?}");
    assert_eq!(decision, CacheDecision::Stale);
    assert_eq!(
        read_cached_if_fresh(Some(dir.path()), ID_FRESH, modified),
        None,
        "stale cache must miss so the file is re-downloaded"
    );

    // ---- 3. NULL modified_time trusts the cache ----------------------------
    set_modified_time(&pool, ID_FRESH, None).await;
    let modified: Option<DateTime<Utc>> =
        sqlx::query_scalar("SELECT modified_time FROM statsbooks WHERE drive_file_id = $1")
            .bind(ID_FRESH)
            .fetch_one(&pool)
            .await
            .unwrap();
    let decision = decide_cache(Some(dir.path()), ID_FRESH, modified);
    println!("NULL modified_time: decision={decision:?}");
    assert_eq!(decision, CacheDecision::Use);

    // ---- 4. Removal deletes the row and its cache file ---------------------
    seed_cache(dir.path(), ID_REMOVED, bytes, base);
    sqlx::query(
        r#"INSERT INTO statsbooks
             (drive_file_id, name, quarter, mime_type, modified_time,
              first_seen, last_seen, parse_status, parser_version, parse_error)
           VALUES ($1, $2, 'test', NULL, $3, '2000-01-01', '2000-01-01', 'parsed', 1, NULL)"#,
    )
    .bind(ID_REMOVED)
    .bind(format!("{ID_REMOVED}.xlsx"))
    .bind(base)
    .execute(&pool)
    .await
    .unwrap();
    let removed_path = cache_path(dir.path(), ID_REMOVED).unwrap();
    assert!(removed_path.is_file(), "cache file seeded");

    // An ancient marker means only rows with last_seen < 2001 can be deleted;
    // concurrent tests' rows use now() and are untouched.
    let marker = Utc.with_ymd_and_hms(2001, 1, 1, 0, 0, 0).unwrap();
    let removed = mark_removed(&pool, marker, Some(dir.path())).await.unwrap();
    println!(
        "mark_removed: removed={removed} row_exists={} cache_file_exists={}",
        row_exists(&pool, ID_REMOVED).await,
        removed_path.exists()
    );
    assert_eq!(removed, 1);
    assert!(!row_exists(&pool, ID_REMOVED).await, "row deleted");
    assert!(!removed_path.exists(), "cache file deleted with the row");

    cleanup(&pool).await;
    let leftover: i64 = sqlx::query_scalar(
        "SELECT count(*)::INT8 FROM statsbooks WHERE drive_file_id = ANY($1::STRING[])",
    )
    .bind(&ALL_IDS[..])
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(leftover, 0, "sentinel rows cleaned up");
    println!("cleanup complete: {leftover} sentinel row(s) left");
}
