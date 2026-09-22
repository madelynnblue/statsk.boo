//! Stage 4 of the WFTDA-first redesign: parse statsbooks into `statsbook_*`.
//!
//! `store_parsed` is deliberately Drive-free — it takes raw `.xlsx` bytes — so
//! the parse+store logic can be exercised without a Google service account.
//! `parse_pending` is the only function that touches Drive: it selects
//! `statsbooks` rows that need parsing, downloads each with the existing
//! service-account `DriveClient`, and delegates to `store_parsed`.
//!
//! Every stage is idempotent. Re-parsing a file replaces its own
//! `statsbook_games`/`statsbook_sides`/`statsbook_skaters`/`statsbook_summary`
//! rows rather than duplicating them. Duplicate Drive uploads are resolved at
//! the game level by `resolve::dedup_matched_games` (newest `modified_time`
//! wins), not here.

use crate::canon::{canonicalize_league, canonicalize_name, canonicalize_team};
use crate::ingest::data_cache;
use crate::ingest::drive::DriveClient;
use crate::ingest::parse::{self, PARSER_VERSION};
use crate::models::{GameData, SideStats};
use anyhow::Result;
use chrono::NaiveDate;
use futures::StreamExt;
use sqlx::{PgPool, Postgres, Transaction};
use std::path::Path;
use tracing::{debug, info, warn};

/// Files downloaded + parsed per `parse_pending` call. The Drive client
/// serializes HTTP, so this only bounds how much work one run takes.
pub const PARSE_BATCH_LIMIT: usize = 500;

/// Bounded concurrency for download+parse within a batch: every available core
/// by default (matching the old ingest), overridable with `PARSE_CONCURRENCY`.
/// The CPU parse itself runs on `spawn_blocking`, so these do not starve the
/// async runtime.
pub fn parse_concurrency() -> usize {
    std::env::var("PARSE_CONCURRENCY")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(4)
        })
}

/// What `store_parsed` did with one statsbook.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseOutcome {
    /// Parsed and stored. `skaters` counts non-empty roster rows across both
    /// sides; `summaries` is 2 when a Game Summary was present, else 0.
    Parsed {
        sides: usize,
        skaters: usize,
        summaries: usize,
    },
    /// The statsbook has no recorded game data (an unfilled placeholder). Its
    /// `parse_status` is set to `skipped` and nothing is stored.
    SkippedEmpty,
}

/// Parse `bytes` with calamine. CPU-bound, so callers that fetched the bytes
/// from the network should run this on `spawn_blocking`.
pub fn parse_statsbook_bytes(
    file_name: &str,
    bytes: &[u8],
) -> Result<(GameData, Option<NaiveDate>)> {
    parse::parse_statsbook_with_date(bytes, Some(file_name))
        .map_err(|e| anyhow::anyhow!("parse error in {file_name}: {e:#}"))
}

/// Parse `bytes` and store the result in the `statsbook_*` tables, keyed by
/// `drive_file_id`.
///
/// `file_name` is passed to the parser as a last-resort date source (the
/// `[WFTDA]STATS-YYYY-MM-DD_...` convention) and used in log/error messages.
/// This is the single-file/test path; `parse_pending` splits the parse onto the
/// blocking pool and calls `store_parsed_game` directly.
pub async fn store_parsed(
    pool: &PgPool,
    drive_file_id: &str,
    file_name: &str,
    bytes: &[u8],
) -> Result<ParseOutcome> {
    let (game, date) = parse_statsbook_bytes(file_name, bytes)?;
    store_parsed_game(pool, drive_file_id, file_name, game, date).await
}

/// Store an already-parsed statsbook. Split from `store_parsed` so the CPU
/// parse can run on the blocking pool without blocking a runtime worker.
pub async fn store_parsed_game(
    pool: &PgPool,
    drive_file_id: &str,
    file_name: &str,
    game: GameData,
    date: Option<NaiveDate>,
) -> Result<ParseOutcome> {
    // Unfilled placeholder: rosters/metadata filled in, no jams and 0–0. Store
    // nothing and never let it reach the matcher or the site.
    if parse::has_no_game_data(&game) {
        mark_skipped(pool, drive_file_id, None).await?;
        info!("{file_name} ({drive_file_id}): no recorded game data, marked skipped");
        return Ok(ParseOutcome::SkippedEmpty);
    }

    let date = date.ok_or_else(|| anyhow::anyhow!("{file_name}: parsed statsbook has no date"))?;

    let game_data = serde_json::to_value(&game)?;
    let home_stats = game
        .game_summary
        .as_ref()
        .map(|s| -> serde_json::Result<_> {
            serde_json::to_value(SideStats {
                players: s.home_players.clone(),
                totals: s.home_totals.clone(),
            })
        })
        .transpose()?;
    let away_stats = game
        .game_summary
        .as_ref()
        .map(|s| -> serde_json::Result<_> {
            serde_json::to_value(SideStats {
                players: s.away_players.clone(),
                totals: s.away_totals.clone(),
            })
        })
        .transpose()?;

    // Roster rows without a name are not searchable identities (same rule as
    // the legacy pipeline). Leading zeros in `number` are preserved because the
    // parser already yields strings.
    let mut skater_rows: Vec<(String, String, String, String, String)> = Vec::new();
    for (side_key, side) in [("home", &game.home), ("away", &game.away)] {
        for skater in &side.skaters {
            if skater.name.is_empty() {
                continue;
            }
            skater_rows.push((
                drive_file_id.to_string(),
                side_key.to_string(),
                skater.number.clone(),
                skater.name.clone(),
                canonicalize_name(&skater.name),
            ));
        }
    }
    let skater_count = skater_rows.len();
    let summaries = usize::from(home_stats.is_some()) + usize::from(away_stats.is_some());

    let mut attempt = 0u32;
    loop {
        let mut tx = pool.begin().await?;

        // Replace any previous parse of this same file (idempotent re-parse).
        delete_parsed_rows(&mut tx, drive_file_id).await?;

        sqlx::query!(
            r#"INSERT INTO statsbook_games
                 (drive_file_id, date, version, home_score, away_score, game_data, parsed_at)
               VALUES ($1, $2, $3, $4, $5, $6, now())"#,
            drive_file_id,
            date,
            &game.version,
            game.home_score,
            game.away_score,
            &game_data,
        )
        .execute(&mut *tx)
        .await?;

        for (side_key, side) in [("home", &game.home), ("away", &game.away)] {
            let league_canonical = canonicalize_league(side.league.as_deref().unwrap_or(""));
            let team_canonical =
                canonicalize_team(side.league.as_deref(), side.team.as_deref().unwrap_or(""));
            sqlx::query!(
                r#"INSERT INTO statsbook_sides
                     (drive_file_id, side, wftda_team_id, league, team, color, league_canonical, team_canonical)
                   VALUES ($1, $2, NULL, $3, $4, $5, $6, $7)"#,
                drive_file_id,
                side_key,
                side.league.as_deref(),
                side.team.as_deref(),
                side.color.as_deref(),
                league_canonical,
                team_canonical,
            )
            .execute(&mut *tx)
            .await?;
        }

        if !skater_rows.is_empty() {
            let mut qb = sqlx::QueryBuilder::new(
                "INSERT INTO statsbook_skaters (drive_file_id, side, number, name, name_canonical) ",
            );
            qb.push_values(
                skater_rows.iter(),
                |mut b, (drive_file_id, side, number, name, name_canonical)| {
                    b.push_bind(drive_file_id)
                        .push_bind(side)
                        .push_bind(number)
                        .push_bind(name)
                        .push_bind(name_canonical);
                },
            );
            qb.build().execute(&mut *tx).await?;
        }

        if let Some(ref home_stats) = home_stats {
            sqlx::query!(
                "INSERT INTO statsbook_summary (drive_file_id, side, stats) VALUES ($1, 'home', $2)",
                drive_file_id,
                home_stats,
            )
            .execute(&mut *tx)
            .await?;
        }
        if let Some(ref away_stats) = away_stats {
            sqlx::query!(
                "INSERT INTO statsbook_summary (drive_file_id, side, stats) VALUES ($1, 'away', $2)",
                drive_file_id,
                away_stats,
            )
            .execute(&mut *tx)
            .await?;
        }

        sqlx::query!(
            "UPDATE statsbooks SET parse_status = 'parsed', parser_version = $2, parse_error = NULL WHERE drive_file_id = $1",
            drive_file_id,
            PARSER_VERSION,
        )
        .execute(&mut *tx)
        .await?;

        match tx.commit().await {
            Ok(()) => break,
            Err(e) => {
                if super::is_retryable(&e) && attempt < 5 {
                    attempt += 1;
                    let delay = std::time::Duration::from_millis(100 * (1 << attempt));
                    warn!(
                        "retryable transaction error storing {file_name}, attempt {attempt}/5, waiting {delay:?}: {e}"
                    );
                    tokio::time::sleep(delay).await;
                    continue;
                }
                return Err(e.into());
            }
        }
    }

    info!(
        "{file_name} ({drive_file_id}): parsed 2 sides, {skater_count} skaters, {summaries} summary row(s)"
    );
    Ok(ParseOutcome::Parsed {
        sides: 2,
        skaters: skater_count,
        summaries,
    })
}

/// Stage 4 driver: select up to `limit` statsbooks that need parsing, reuse the
/// on-disk byte cache when it is fresh, otherwise download each with `drive`
/// and write the bytes through to `cache_dir`, then store it. Failures are
/// recorded on the row (`parse_status='failed'`, `parse_error`) and never abort
/// the batch.
///
/// `cache_dir` is `None` when `GAME_DATA_DIR` is unset (always download). A
/// cache write failure is logged and ignored: the parse already has the bytes.
/// Returns `(selected, succeeded)`: how many rows the batch selected and how
/// many reached a terminal state. A row that fails is retried on the next
/// cycle, so a caller draining batches must stop when a full batch succeeds on
/// nothing (otherwise permanently failing rows would spin forever).
pub async fn parse_pending(
    pool: &PgPool,
    drive: &DriveClient,
    limit: usize,
    cache_dir: Option<&Path>,
) -> (usize, usize) {
    let rows = match sqlx::query!(
        r#"SELECT drive_file_id, name, mime_type, modified_time
           FROM statsbooks
           WHERE parse_status IN ('pending', 'failed')
              OR parser_version < $1
           ORDER BY modified_time ASC NULLS FIRST, drive_file_id
           LIMIT $2"#,
        PARSER_VERSION,
        limit as i64,
    )
    .fetch_all(pool)
    .await
    {
        Ok(rows) => rows,
        Err(e) => {
            warn!("parse stage: selecting pending statsbooks failed: {e:#}");
            return (0, 0);
        }
    };

    if rows.is_empty() {
        return (0, 0);
    }
    let selected = rows.len();
    let concurrency = parse_concurrency();
    info!(
        "parse stage: {} statsbook(s) to parse (cache dir: {}, concurrency: {concurrency})",
        selected,
        cache_dir.map_or_else(|| "disabled".to_string(), |d| d.display().to_string()),
    );

    let pool = pool.clone();
    let succeeded = futures::stream::iter(rows)
        .map(|row| {
            let pool = pool.clone();
            async move {
                let drive_file_id = row.drive_file_id;
                let name = row.name;
                let mime_type = row.mime_type;
                let modified_time = row.modified_time;

                // Cache first: a hit on a PARSER_VERSION bump avoids the Drive
                // download entirely because `modified_time` is unchanged.
                let cached = data_cache::read_cached_if_fresh(
                    cache_dir,
                    &drive_file_id,
                    modified_time,
                );
                let from_cache = cached.is_some();
                let bytes = match cached {
                    Some(bytes) => bytes,
                    None => {
                        match drive
                            .download_file(&drive_file_id, mime_type.as_deref())
                            .await
                        {
                            Ok(bytes) => {
                                // Write-through: best-effort, never fails the parse.
                                if let Some(dir) = cache_dir {
                                    if let Err(e) =
                                        data_cache::write_cached(dir, &drive_file_id, &bytes)
                                    {
                                        warn!(
                                            "cache write for {name} ({drive_file_id}) failed (continuing): {e:#}"
                                        );
                                    }
                                }
                                bytes
                            }
                            Err(e) => {
                                warn!("download of {name} ({drive_file_id}) failed: {e:#}");
                                set_failed(&pool, &drive_file_id, &format!("{e:#}")).await;
                                return false;
                            }
                        }
                    }
                };

                if from_cache {
                    debug!("parse stage: using cached bytes for {name} ({drive_file_id})");
                }

                let parse_name = name.clone();
                let parsed =
                    tokio::task::spawn_blocking(move || parse_statsbook_bytes(&parse_name, &bytes))
                        .await;
                let (game, date) = match parsed {
                    Ok(Ok(parsed)) => parsed,
                    Ok(Err(e)) => {
                        warn!("parse of {name} ({drive_file_id}) failed: {e:#}");
                        set_failed(&pool, &drive_file_id, &format!("{e:#}")).await;
                        return false;
                    }
                    Err(e) => {
                        warn!("parser task for {name} ({drive_file_id}) panicked: {e}");
                        set_failed(
                            &pool,
                            &drive_file_id,
                            &format!("parser task panicked: {e}"),
                        )
                        .await;
                        return false;
                    }
                };

                match store_parsed_game(&pool, &drive_file_id, &name, game, date).await {
                    Ok(outcome) => {
                        debug!("parsed {name} ({drive_file_id}): {outcome:?}");
                        true
                    }
                    Err(e) => {
                        warn!("store of {name} ({drive_file_id}) failed: {e:#}");
                        set_failed(&pool, &drive_file_id, &format!("{e:#}")).await;
                        false
                    }
                }
            }
        })
        .buffer_unordered(concurrency)
        .fold(0usize, |acc, ok| async move { acc + ok as usize })
        .await;
    info!("parse stage complete: {selected} selected, {succeeded} succeeded");
    (selected, succeeded)
}

/// Deletes this file's parsed rows. `statsbook_sides` is keyed by
/// `statsbooks` (not `statsbook_games`), so it must be removed explicitly;
/// deleting it cascades to `statsbook_skaters`, and deleting `statsbook_games`
/// cascades to `statsbook_summary`.
async fn delete_parsed_rows(tx: &mut Transaction<'_, Postgres>, drive_file_id: &str) -> Result<()> {
    sqlx::query!(
        "DELETE FROM statsbook_sides WHERE drive_file_id = $1",
        drive_file_id
    )
    .execute(&mut **tx)
    .await?;
    sqlx::query!(
        "DELETE FROM statsbook_summary WHERE drive_file_id = $1",
        drive_file_id
    )
    .execute(&mut **tx)
    .await?;
    sqlx::query!(
        "DELETE FROM statsbook_games WHERE drive_file_id = $1",
        drive_file_id
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Marks a statsbook `skipped`, clearing any parsed rows it previously had.
/// `reason`, when present, is recorded in `parse_error` for audit.
async fn mark_skipped(pool: &PgPool, drive_file_id: &str, reason: Option<&str>) -> Result<()> {
    let mut tx = pool.begin().await?;
    delete_parsed_rows(&mut tx, drive_file_id).await?;
    sqlx::query!(
        "UPDATE statsbooks SET parse_status = 'skipped', parser_version = $2, parse_error = $3 WHERE drive_file_id = $1",
        drive_file_id,
        PARSER_VERSION,
        reason,
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Records a parse/download failure on the row. Best-effort: a failure to
/// record is logged, not propagated, so one bad file never aborts a batch.
async fn set_failed(pool: &PgPool, drive_file_id: &str, error: &str) {
    if let Err(e) = sqlx::query!(
        "UPDATE statsbooks SET parse_status = 'failed', parse_error = $2 WHERE drive_file_id = $1",
        drive_file_id,
        error,
    )
    .execute(pool)
    .await
    {
        warn!("failed to record parse failure for {drive_file_id}: {e:#}");
    }
}
