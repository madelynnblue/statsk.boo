pub mod data_cache;
pub mod drive;
pub mod drive_auth;
pub mod parse;
pub mod resolve;
pub mod statsbook_index;
pub mod statsbook_parse;
pub mod wftda;

use crate::cache::Cache;
use crate::config::Config;
use drive::DriveClient;
use sqlx::PgPool;
use std::sync::Arc;
use tracing::{info, warn};

/// Background loop for the WFTDA reference sync (stages 1-2). Runs once
/// immediately, then sleeps for `cfg.wftda_sync_interval`. The HTTP response
/// cache is cleared afterwards: the sync upserts teams/games and runs resolve,
/// so pages cached before it started can be stale.
pub async fn wftda_sync_loop(cfg: Arc<Config>, pool: Arc<PgPool>, cache: Arc<Cache>) {
    info!(
        "WFTDA sync loop starting (interval {:?})",
        cfg.wftda_sync_interval
    );
    loop {
        wftda::run_sync(
            &pool,
            cfg.wftda_games_lookback,
            cfg.wftda_full_refresh_interval,
        )
        .await;
        cache.clear();
        tokio::time::sleep(cfg.wftda_sync_interval).await;
    }
}

/// Background loop for the Drive statsbook index (stage 3) and the statsbook
/// parse stage (stage 4), followed by the game resolver (stages 5-6). Needs the
/// service account; without one it logs and returns instead of panicking, so
/// offline setups keep working. The HTTP response cache is cleared after every
/// resolve so a page cached before a statsbook was parsed (or retracted) does
/// not keep serving score-only/stale HTML.
pub async fn drive_index_loop(cfg: Arc<Config>, pool: Arc<PgPool>, cache: Arc<Cache>) {
    let Some(sa) = cfg.service_account.clone() else {
        warn!("Drive statsbook index disabled: no service account configured");
        return;
    };
    let drive = DriveClient::new(sa);
    // The on-disk byte cache (`GAME_DATA_DIR`) lets a PARSER_VERSION bump
    // re-parse without re-downloading; absent, every parse downloads.
    let cache_dir = cfg.game_data_dir.as_deref();
    info!(
        "Drive statsbook index loop starting (interval {:?}, cache dir: {})",
        cfg.drive_index_interval,
        cache_dir.map_or_else(|| "disabled".to_string(), |d| d.display().to_string()),
    );

    // Drain any backlog left by a previous run before the first listing, so a
    // fresh backfill or parser bump does not wait one batch per interval.
    let drained = drain_pending(&pool, &drive, cache_dir, cache.as_ref()).await;
    if drained > 0 {
        info!("parse stage: drained {drained} statsbook(s) before first index");
    }
    // Stage 5-6: match what was just parsed.
    resolve::run_resolve(&pool).await;
    cache.clear();

    loop {
        match statsbook_index::sync_statsbook_index(
            &pool,
            &drive,
            &cfg.google_drive_folder_id,
            cache_dir,
        )
        .await
        {
            Ok(stats) => info!(
                "Drive statsbook index complete: {} seen, {} removed",
                stats.seen, stats.removed
            ),
            Err(e) => warn!("Drive statsbook index failed: {e:#}"),
        }
        // Stage 4: parse rows marked pending, draining every batch so a first
        // backfill does not trickle in one batch per interval.
        let parsed = drain_pending(&pool, &drive, cache_dir, cache.as_ref()).await;
        if parsed > 0 {
            info!("parse stage: parsed {parsed} statsbook(s) this cycle");
        }
        // Stage 5-6: resolve the newly parsed statsbooks.
        resolve::run_resolve(&pool).await;
        // Pages rendered before this cycle (score-only, old scores, a
        // retracted statsbook) are stale now that the writes have landed.
        cache.clear();
        tokio::time::sleep(cfg.drive_index_interval).await;
    }
}

/// Drain the pending-parse queue in bounded batches. A fresh database or a
/// `PARSER_VERSION` bump can leave thousands of rows pending, and one batch per
/// `DRIVE_INDEX_INTERVAL` would trickle them in over days. Stops when a batch is
/// short (queue empty) or a full batch succeeded on nothing (only permanently
/// failing rows remain; they are retried next cycle).
async fn drain_pending(
    pool: &PgPool,
    drive: &DriveClient,
    cache_dir: Option<&std::path::Path>,
    cache: &Cache,
) -> usize {
    let mut succeeded = 0usize;
    loop {
        let (selected, ok) = statsbook_parse::parse_pending(
            pool,
            drive,
            statsbook_parse::PARSE_BATCH_LIMIT,
            cache_dir,
        )
        .await;
        succeeded += ok;
        // Refresh progressively during a long backfill: a page rendered while
        // batch N was running is already stale once batch N commits.
        if ok > 0 {
            cache.clear();
        }
        if selected < statsbook_parse::PARSE_BATCH_LIMIT || ok == 0 {
            break;
        }
    }
    succeeded
}

/// Whether a database error is a CockroachDB retryable serialization failure.
/// Shared by the parse-stage write loop.
pub(crate) fn is_retryable(e: &sqlx::Error) -> bool {
    if let sqlx::Error::Database(db_err) = e {
        let code = db_err.code();
        return code.as_deref() == Some("40001") // serialization_failure
            || code.as_deref() == Some("CR000"); // crdb retry
    }
    false
}
