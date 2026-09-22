//! Drive statsbook index (redesign stage 3).
//!
//! Lists every file in the quarter folders under the configured Drive root
//! using the existing service-account `DriveClient`, upserts one `statsbooks`
//! row per Drive file, and deletes rows for files that have vanished (decision
//! 9: removal is retraction, not soft-delete). `modified_time` is the exact
//! RFC3339 `modifiedTime` from the Drive API, so a re-upload to the same file
//! id is detectable.

use crate::ingest::data_cache;
use crate::ingest::drive::DriveClient;
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use sqlx::PgPool;
use std::path::Path;
use tracing::info;

#[derive(Debug, Default, Clone, Copy)]
pub struct IndexStats {
    /// Files seen in the Drive listing.
    pub seen: usize,
    /// Rows upserted (== seen).
    pub upserted: usize,
    /// Rows deleted because they were not seen in this listing.
    pub removed: u64,
}

/// Stage 3: index the quarter folders under `root_folder_id` into `statsbooks`.
///
/// On success, files absent from the listing are deleted (and their on-disk
/// cache files removed). On any listing or upsert failure the function returns
/// early, so a partial/failed Drive read never triggers deletions.
pub async fn sync_statsbook_index(
    pool: &PgPool,
    drive: &DriveClient,
    root_folder_id: &str,
    cache_dir: Option<&Path>,
) -> Result<IndexStats> {
    // Marker from the DB clock (the same clock the upserts' `now()` uses):
    // rows seen by this run get `last_seen > marker`, everything older has
    // vanished from Drive.
    let marker: DateTime<Utc> = sqlx::query_scalar!("SELECT now() AS marker")
        .fetch_one(pool)
        .await?
        .expect("now() is never NULL");

    let tree = drive
        .list_tree_with_paths(root_folder_id)
        .await
        .context("listing Drive statsbook tree")?;

    let mut seen = 0usize;
    for (path, file) in &tree {
        let modified_time = DateTime::parse_from_rfc3339(&file.modified_time)
            .with_context(|| {
                format!(
                    "parsing modifiedTime {:?} for Drive file {}",
                    file.modified_time, file.name
                )
            })?
            .with_timezone(&Utc);
        let quarter = quarter_of(path);
        upsert_statsbook(
            pool,
            &file.id,
            &file.name,
            quarter,
            file.mime_type.as_deref(),
            modified_time,
        )
        .await
        .with_context(|| format!("upserting statsbook {} ({})", file.name, file.id))?;
        seen += 1;
    }

    let removed = mark_removed(pool, marker, cache_dir).await?;

    let stats = IndexStats {
        seen,
        upserted: seen,
        removed,
    };
    info!(
        "statsbooks index: {} seen, {} upserted, {} removed",
        stats.seen, stats.upserted, stats.removed
    );
    Ok(stats)
}

async fn upsert_statsbook(
    pool: &PgPool,
    drive_file_id: &str,
    name: &str,
    quarter: Option<&str>,
    mime_type: Option<&str>,
    modified_time: DateTime<Utc>,
) -> Result<()> {
    sqlx::query!(
        r#"INSERT INTO statsbooks
             (drive_file_id, name, quarter, mime_type, modified_time,
              first_seen, last_seen, parse_status, parser_version)
           VALUES ($1, $2, $3, $4, $5, now(), now(), 'pending', NULL)
           ON CONFLICT (drive_file_id) DO UPDATE SET
             name           = EXCLUDED.name,
             quarter        = EXCLUDED.quarter,
             mime_type      = EXCLUDED.mime_type,
             modified_time  = EXCLUDED.modified_time,
             last_seen      = now(),
             -- A new or re-uploaded file needs (re-)parsing; an unchanged one
             -- keeps whatever the parser stage already recorded.
             parse_status   = CASE
                 WHEN statsbooks.modified_time IS DISTINCT FROM EXCLUDED.modified_time
                 THEN 'pending'
                 ELSE statsbooks.parse_status
             END,
             parser_version = CASE
                 WHEN statsbooks.modified_time IS DISTINCT FROM EXCLUDED.modified_time
                 THEN NULL
                 ELSE statsbooks.parser_version
             END"#,
        drive_file_id,
        name,
        quarter,
        mime_type,
        modified_time,
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Deletes statsbooks whose `last_seen` predates `seen_before` (i.e. rows not
/// observed in the current Drive listing), and removes their cached bytes
/// (decision 9: removal is retraction). Returns the number of rows removed.
pub async fn mark_removed(
    pool: &PgPool,
    seen_before: DateTime<Utc>,
    cache_dir: Option<&Path>,
) -> Result<u64> {
    // Collect the ids before the DELETE: the statsbooks row is the only thing
    // that maps a Drive file id to its cache file.
    let doomed = sqlx::query_scalar!(
        "SELECT drive_file_id FROM statsbooks WHERE last_seen < $1",
        seen_before,
    )
    .fetch_all(pool)
    .await?;

    let result = sqlx::query!("DELETE FROM statsbooks WHERE last_seen < $1", seen_before,)
        .execute(pool)
        .await?;
    let removed = result.rows_affected();

    if let Some(dir) = cache_dir {
        for drive_file_id in &doomed {
            data_cache::remove_cached(dir, drive_file_id);
        }
    }

    if removed > 0 {
        info!("statsbooks: deleted {removed} row(s) no longer present on Drive");
    }
    Ok(removed)
}

/// The quarter folder is the first path segment produced by
/// `list_tree_with_paths` ("2026-Q3/[WFTDA]STATS-....xlsx").
fn quarter_of(path: &str) -> Option<&str> {
    path.split_once('/')
        .map(|(quarter, _)| quarter)
        .filter(|q| !q.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quarter_from_path() {
        assert_eq!(
            quarter_of("2026-Q3/[WFTDA]STATS-2026-08-09_x_vs_y.xlsx"),
            Some("2026-Q3")
        );
        assert_eq!(quarter_of("2023-Q1/file.xlsx"), Some("2023-Q1"));
        assert_eq!(quarter_of("/leading/file.xlsx"), None);
        assert_eq!(quarter_of("no-slash.xlsx"), None);
    }

    #[test]
    fn parses_drive_modified_time() {
        let t = DateTime::parse_from_rfc3339("2023-01-21T18:22:04.000Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(t.to_rfc3339(), "2023-01-21T18:22:04+00:00");
        assert!(DateTime::parse_from_rfc3339("2023-01-21").is_err());
    }
}
