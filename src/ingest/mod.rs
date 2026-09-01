pub mod data_cache;
pub mod drive;
pub mod drive_auth;
pub mod parse;

use crate::cache::Cache;
use crate::canon::{canonicalize_league, canonicalize_name, canonicalize_team};
use crate::config::Config;
use crate::models::GameData;
use anyhow::Context;
use chrono::{DateTime, NaiveDate, Utc};
use drive::DriveClient;
use drive::DriveFile;
use futures::StreamExt;
use futures::stream::FuturesOrdered;
use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing::{error, info, warn};

struct LocalSource {
    root: PathBuf,
}

impl LocalSource {
    fn new(root: PathBuf) -> Self {
        Self { root }
    }

    fn list_all_xlsx(&self) -> anyhow::Result<Vec<DriveFile>> {
        let mut files = Vec::new();
        visit_dir(&self.root, &self.root, &mut files)?;
        Ok(files)
    }

    fn read_file(&self, relative_path: &str) -> anyhow::Result<Vec<u8>> {
        let path = self.root.join(relative_path);
        std::fs::read(&path).with_context(|| format!("reading {}", path.display()))
    }
}

fn visit_dir(
    root: &std::path::Path,
    dir: &std::path::Path,
    files: &mut Vec<DriveFile>,
) -> anyhow::Result<()> {
    for entry in std::fs::read_dir(dir).with_context(|| format!("reading dir {}", dir.display()))? {
        let entry = entry.with_context(|| format!("entry in {}", dir.display()))?;
        let path = entry.path();
        if path.is_dir() {
            visit_dir(root, &path, files)?;
        } else if path.extension().is_some_and(|e| e == "xlsx") {
            files.push(DriveFile::from_local(&path, root)?);
        }
    }
    Ok(())
}

enum FileSource {
    Drive(DriveClient),
    Local(LocalSource),
}

impl FileSource {
    fn source_str(&self) -> &'static str {
        match self {
            FileSource::Drive(_) => "drive",
            FileSource::Local(_) => "file",
        }
    }

    async fn list_xlsx_since(
        &self,
        folder_id: &str,
        since: &str,
    ) -> anyhow::Result<Vec<DriveFile>> {
        match self {
            FileSource::Drive(c) => c.list_xlsx_since(folder_id, since).await,
            FileSource::Local(s) => s.list_all_xlsx(),
        }
    }

    /// For the Drive variant, lists all XLSX files in `folder_id`. For the Local variant, `folder_id` is ignored — all `.xlsx` files under the configured root are returned.
    async fn list_all_xlsx(&self, folder_id: &str) -> anyhow::Result<Vec<DriveFile>> {
        match self {
            FileSource::Drive(c) => c.list_all_xlsx(folder_id).await,
            FileSource::Local(s) => s.list_all_xlsx(),
        }
    }

    async fn read_file(&self, file_id: &str, mime_type: Option<&str>) -> anyhow::Result<Vec<u8>> {
        match self {
            FileSource::Drive(c) => c.download_file(file_id, mime_type).await,
            FileSource::Local(s) => s.read_file(file_id),
        }
    }
}

pub async fn ingest_loop(cfg: Arc<Config>, pool: Arc<PgPool>, cache: Arc<Cache>) {
    info!("ingest loop starting");
    let source = Arc::new(if let Some(ref dir) = cfg.ingest_dir {
        let root = PathBuf::from(dir);
        if !root.is_dir() {
            panic!("INGEST_DIR is not a directory: {dir}");
        }
        FileSource::Local(LocalSource::new(root))
    } else {
        FileSource::Drive(DriveClient::new(
            cfg.service_account
                .clone()
                .expect("service account required when INGEST_DIR not set"),
        ))
    });

    // Serializes DB transactions within this process: no two commits run
    // concurrently, so the ingest path never trips over its own concurrent
    // commits. The canonical-duplicate check is a point lookup on the unique
    // canonical_id index (no full-table scan), and no RETRY_SERIALIZABLE retries
    // have been observed in practice, so this is cheap insurance rather than a
    // necessity. Cross-process conflicts (e.g. rolling restart) are handled by
    // CockroachDB's serializable isolation via the is_retryable retry loop in
    // commit_file — tx_sem provides no cross-process guarantee.
    // `run_ingest` commits in oldest-first order so MAX(modified_time) is a monotonic
    // cursor: a crash leaves it pointing to the last committed file, and all remaining
    // files have a larger modifiedTime and will be picked up on the next run.
    let tx_sem = Arc::new(tokio::sync::Semaphore::new(1));

    match reingest_stale(
        pool.clone(),
        Some(source.clone()),
        tx_sem.clone(),
        cfg.game_data_dir.clone(),
        false,
    )
    .await
    {
        Ok(n) if n > 0 => {
            info!("re-ingested {n} stale game(s)");
            cache.clear();
        }
        Err(e) => error!("re-ingest of stale games failed: {e:#}"),
        _ => {}
    }

    match reconcile_missing(&cfg, pool.clone(), source.clone(), tx_sem.clone()).await {
        Ok(n) if n > 0 => {
            info!("reconciled {n} missing game(s)");
            cache.clear();
        }
        Err(e) => error!("reconciliation failed: {e:#}"),
        _ => {}
    }

    loop {
        match run_ingest(&cfg, pool.clone(), source.clone(), tx_sem.clone()).await {
            Ok(n) if n > 0 => {
                cache.clear();
            }
            Ok(_) => {}
            Err(e) => error!("ingest run failed: {e:#}"),
        }
        tokio::time::sleep(cfg.ingest_interval).await;
    }
}

async fn run_ingest(
    cfg: &Config,
    pool: Arc<PgPool>,
    source: Arc<FileSource>,
    tx_sem: Arc<tokio::sync::Semaphore>,
) -> anyhow::Result<usize> {
    let last_ingest = last_ingest_at(&pool).await?.unwrap_or(
        chrono::NaiveDate::from_ymd_opt(2023, 1, 1)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc(),
    );

    let jitter = chrono::Duration::from_std(cfg.ingest_jitter).unwrap_or_default();
    let since = (last_ingest - jitter).to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    info!("ingesting files since {since}");
    let mut files = source
        .list_xlsx_since(&cfg.google_drive_folder_id, &since)
        .await?;

    // Sort oldest-first by Drive modifiedTime so the DB cursor (MAX(modified_time))
    // advances monotonically; if ingestion is interrupted, the next run picks up
    // exactly where it left off rather than missing files with old modifiedTimes.
    files.sort_unstable_by(|a, b| a.modified_time.cmp(&b.modified_time));

    let n = files.len();
    info!("found {n} candidate file(s)");

    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    let prep_sem = Arc::new(tokio::sync::Semaphore::new(cores));
    let cache_dir = cfg.game_data_dir.clone();

    // Spawn one prepare task per file (download + parse + read-only DB checks).
    // Tasks are independent Tokio tasks, so they run concurrently even while commits
    // are in progress. FuturesOrdered yields their JoinHandles in oldest-first order,
    // so the commit loop below processes files in that same order.
    let mut ordered: FuturesOrdered<_> = files
        .into_iter()
        .map(|file| {
            let pool = pool.clone();
            let source = source.clone();
            let prep_sem = prep_sem.clone();
            let cache_dir = cache_dir.clone();
            tokio::spawn(async move {
                let _permit = prep_sem
                    .acquire_owned()
                    .await
                    .expect("prep semaphore closed");
                let name = file.name.clone();
                let result = prepare_file(&pool, &source, &file, cache_dir.as_deref()).await;
                (name, result)
            })
        })
        .collect();

    let mut ingested = 0;
    let mut i = 0usize;
    while let Some(join_result) = ordered.next().await {
        i += 1;
        let (name, prep_result) = match join_result {
            Ok(r) => r,
            Err(e) => {
                warn!("prepare task panicked ({i}/{n}): {e}");
                continue;
            }
        };
        match prep_result {
            Err(e) => warn!("skipping {name}: {e:#}"),
            Ok(None) => {}
            Ok(Some(prep)) => match commit_file(&pool, &tx_sem, prep).await {
                Ok(()) => {
                    info!("ingested ({i}/{n}) {name}");
                    ingested += 1;
                }
                Err(e) => warn!("skipping {name}: {e:#}"),
            },
        }
    }
    if ingested > 0 {
        info!("ingested {ingested} new game(s)");
    }
    Ok(ingested)
}

async fn last_ingest_at(pool: &PgPool) -> anyhow::Result<Option<chrono::DateTime<chrono::Utc>>> {
    let row = sqlx::query!("SELECT MAX(modified_time) as ts FROM games")
        .fetch_one(pool)
        .await?;
    Ok(row.ts)
}

struct PreparedInsert {
    file_id: String,
    file_name: String,
    modified_time: DateTime<Utc>,
    source_str: String,
    file_exists: bool,
    canonical_duplicate_id: Option<String>,
    date: Option<NaiveDate>,
    game: GameData,
    canonical_id: String,
    fingerprint_json: serde_json::Value,
    game_data: serde_json::Value,
    home_stats: Option<(serde_json::Value, serde_json::Value)>,
}

async fn prepare_file(
    pool: &PgPool,
    source: &FileSource,
    file: &DriveFile,
    cache_dir: Option<&Path>,
) -> anyhow::Result<Option<PreparedInsert>> {
    let modified_time: DateTime<Utc> = file.modified_time.parse()?;

    // Skip if we already have this file at this (or a newer) modified time.
    let existing = sqlx::query!("SELECT modified_time FROM games WHERE id = $1", file.id)
        .fetch_optional(pool)
        .await?;
    let file_exists = existing.is_some();
    if let Some(existing) = existing
        && existing.modified_time >= modified_time
    {
        return Ok(None);
    }

    let bytes = source
        .read_file(&file.id, file.mime_type.as_deref())
        .await?;
    let (game, date) = parse::parse_statsbook_with_date(&bytes, Some(&file.name))
        .map_err(|e| anyhow::anyhow!("parse error in {}: {e:#}", file.name))?;

    if let Some(ref d) = date {
        let tomorrow = chrono::Utc::now().date_naive() + chrono::Duration::days(1);
        if *d > tomorrow {
            info!(
                "skipping {}: date {d} is more than 1 day in the future",
                file.name
            );
            return Ok(None);
        }
    }

    let fingerprint = match build_fingerprint(&game, date) {
        Ok(fp) => fp,
        Err(e) => {
            warn!("skipping {}: cannot build fingerprint: {e}", file.name);
            return Ok(None);
        }
    };
    let canonical_id = compute_canonical_id(&fingerprint);
    if let Some(dir) = cache_dir {
        data_cache::write_game_data(dir, &canonical_id, &bytes)
            .map_err(|e| anyhow::anyhow!("cache write failed for {}: {e:#}", file.name))?;
    }
    let fingerprint_json = serde_json::to_value(&fingerprint)?;
    let game_data = serde_json::to_value(&game)?;
    let home_stats = game
        .game_summary
        .as_ref()
        .map(|s| -> serde_json::Result<_> {
            Ok((
                serde_json::to_value(&crate::models::SideStats {
                    players: s.home_players.clone(),
                    totals: s.home_totals.clone(),
                })?,
                serde_json::to_value(&crate::models::SideStats {
                    players: s.away_players.clone(),
                    totals: s.away_totals.clone(),
                })?,
            ))
        })
        .transpose()?;

    // Check for a canonical duplicate concurrently with other files' prepare tasks
    // rather than serialized behind transaction WAL flushes.
    let canonical_duplicate = sqlx::query!(
        "SELECT id, modified_time FROM games WHERE canonical_id = $1",
        canonical_id,
    )
    .fetch_optional(pool)
    .await?;

    let canonical_duplicate_id = if let Some(ref dup) = canonical_duplicate {
        if dup.modified_time >= modified_time {
            info!(
                "duplicate game detected: skipping {} (existing {} has same or newer modified time)",
                file.id, dup.id
            );
            return Ok(None);
        }
        info!(
            "duplicate game detected: replacing {} with {} (newer modified time)",
            dup.id, file.id
        );
        Some(dup.id.clone())
    } else {
        None
    };

    Ok(Some(PreparedInsert {
        file_id: file.id.clone(),
        file_name: file.name.clone(),
        modified_time,
        source_str: source.source_str().to_string(),
        file_exists,
        canonical_duplicate_id,
        date,
        game,
        canonical_id,
        fingerprint_json,
        game_data,
        home_stats,
    }))
}

async fn commit_file(
    pool: &PgPool,
    tx_sem: &tokio::sync::Semaphore,
    prep: PreparedInsert,
) -> anyhow::Result<()> {
    let _tx_permit = tx_sem.acquire().await?;
    let mut attempt = 0;
    loop {
        let mut tx = pool.begin().await?;

        if prep.file_exists {
            sqlx::query!("DELETE FROM games WHERE id = $1", prep.file_id)
                .execute(&mut *tx)
                .await?;
        }

        if let Some(ref dup_id) = prep.canonical_duplicate_id {
            sqlx::query!("DELETE FROM games WHERE id = $1", dup_id)
                .execute(&mut *tx)
                .await?;
        }

        sqlx::query!(
            r#"INSERT INTO games
               (id, source, date, parser_version, version, game_data,
                modified_time, fingerprint, canonical_id)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)"#,
            prep.file_id,
            prep.source_str,
            prep.date,
            parse::PARSER_VERSION,
            prep.game.version,
            &prep.game_data,
            prep.modified_time,
            &prep.fingerprint_json,
            prep.canonical_id,
        )
        .execute(&mut *tx)
        .await?;

        let mut skater_rows: Vec<(String, String, String, String, String)> = Vec::new();
        for (side_key, side) in [("home", &prep.game.home), ("away", &prep.game.away)] {
            let league_canonical = canonicalize_league(side.league.as_deref().unwrap_or(""));
            let team_canonical =
                canonicalize_team(side.league.as_deref(), side.team.as_deref().unwrap_or(""));
            sqlx::query!(
                "INSERT INTO game_sides (game_id, side, league, team, color, league_canonical, team_canonical) VALUES ($1, $2, $3, $4, $5, $6, $7)",
                prep.file_id,
                side_key,
                side.league,
                side.team,
                side.color,
                league_canonical,
                team_canonical,
            )
            .execute(&mut *tx)
            .await?;

            for skater in &side.skaters {
                // Roster rows without a name are not searchable identities.
                if skater.name.is_empty() {
                    continue;
                }
                skater_rows.push((
                    prep.file_id.clone(),
                    side_key.to_string(),
                    skater.number.clone(),
                    skater.name.clone(),
                    canonicalize_name(&skater.name),
                ));
            }
        }
        if !skater_rows.is_empty() {
            let mut qb = sqlx::QueryBuilder::new(
                "INSERT INTO game_skaters (game_id, side, number, name, name_canonical) ",
            );
            qb.push_values(
                skater_rows,
                |mut b, (game_id, side, number, name, name_canonical)| {
                    b.push_bind(game_id)
                        .push_bind(side)
                        .push_bind(number)
                        .push_bind(name)
                        .push_bind(name_canonical);
                },
            );
            qb.build().execute(&mut *tx).await?;
        }

        if let Some((ref home_stats, ref away_stats)) = prep.home_stats {
            sqlx::query!(
                "INSERT INTO game_summary (game_id, side, stats) VALUES ($1, 'home', $2)",
                prep.file_id,
                home_stats,
            )
            .execute(&mut *tx)
            .await?;
            sqlx::query!(
                "INSERT INTO game_summary (game_id, side, stats) VALUES ($1, 'away', $2)",
                prep.file_id,
                away_stats,
            )
            .execute(&mut *tx)
            .await?;
        }

        match tx.commit().await {
            Ok(()) => return Ok(()),
            Err(e) => {
                if is_retryable(&e) && attempt < 5 {
                    attempt += 1;
                    let delay = std::time::Duration::from_millis(100 * (1 << attempt));
                    warn!(
                        "retryable transaction error for {}, attempt {attempt}/5, waiting {delay:?}: {e}",
                        prep.file_name
                    );
                    tokio::time::sleep(delay).await;
                    continue;
                }
                return Err(e.into());
            }
        }
    }
}

#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct GameFingerprint {
    date: NaiveDate,
    home_league: String,
    home_team: String,
    away_league: String,
    away_team: String,
    home_score: i16,
    away_score: i16,
}

pub fn compute_canonical_id(fp: &GameFingerprint) -> String {
    let home_league = canonicalize_league(&fp.home_league);
    let home_team = canonicalize_team(Some(&fp.home_league), &fp.home_team);
    let away_league = canonicalize_league(&fp.away_league);
    let away_team = canonicalize_team(Some(&fp.away_league), &fp.away_team);
    let input = format!(
        "{}\n{}\n{}\n{}\n{}\n{}\n{}",
        fp.date, home_league, home_team, away_league, away_team, fp.home_score, fp.away_score
    );
    let hash = Sha256::digest(input.as_bytes());
    hex::encode(&hash[..4])
}

pub fn build_fingerprint(
    game: &GameData,
    date: Option<NaiveDate>,
) -> anyhow::Result<GameFingerprint> {
    Ok(GameFingerprint {
        date: date.ok_or_else(|| anyhow::anyhow!("missing date"))?,
        home_league: game
            .home
            .league
            .clone()
            .ok_or_else(|| anyhow::anyhow!("missing home league"))?,
        home_team: game
            .home
            .team
            .clone()
            .ok_or_else(|| anyhow::anyhow!("missing home team"))?,
        away_league: game
            .away
            .league
            .clone()
            .ok_or_else(|| anyhow::anyhow!("missing away league"))?,
        away_team: game
            .away
            .team
            .clone()
            .ok_or_else(|| anyhow::anyhow!("missing away team"))?,
        home_score: game.home_score,
        away_score: game.away_score,
    })
}

// Used by reingest_stale: re-parses a game regardless of what's in the DB;
// uses cached bytes when `data` is Some, otherwise downloads.
async fn insert_parsed_file(
    pool: &PgPool,
    source: Option<&FileSource>,
    source_str: &str,
    file_id: &str,
    file_name: &str,
    modified_time: DateTime<Utc>,
    tx_sem: &tokio::sync::Semaphore,
    file_exists: bool,
    data: Option<Vec<u8>>,
    cache_dir: Option<&Path>,
) -> anyhow::Result<()> {
    let bytes = match data {
        Some(b) => b,
        None => {
            source
                .context("no cached data and no ingest source to download from")?
                .read_file(file_id, None)
                .await?
        }
    };
    let (game, date) = parse::parse_statsbook_with_date(&bytes, Some(file_name))
        .map_err(|e| anyhow::anyhow!("parse error in {file_name}: {e:#}"))?;

    if let Some(ref d) = date {
        let tomorrow = chrono::Utc::now().date_naive() + chrono::Duration::days(1);
        if *d > tomorrow {
            info!("skipping {file_name}: date {d} is more than 1 day in the future");
            return Ok(());
        }
    }

    let fingerprint = match build_fingerprint(&game, date) {
        Ok(fp) => fp,
        Err(e) => {
            warn!("skipping {file_name}: cannot build fingerprint: {e}");
            return Ok(());
        }
    };
    let canonical_id = compute_canonical_id(&fingerprint);
    if let Some(dir) = cache_dir {
        data_cache::write_game_data(dir, &canonical_id, &bytes)
            .map_err(|e| anyhow::anyhow!("cache write failed for {file_name}: {e:#}"))?;
    }
    let fingerprint_json = serde_json::to_value(&fingerprint)?;
    let game_data = serde_json::to_value(&game)?;
    let home_stats = game
        .game_summary
        .as_ref()
        .map(|s| -> serde_json::Result<_> {
            Ok((
                serde_json::to_value(&crate::models::SideStats {
                    players: s.home_players.clone(),
                    totals: s.home_totals.clone(),
                })?,
                serde_json::to_value(&crate::models::SideStats {
                    players: s.away_players.clone(),
                    totals: s.away_totals.clone(),
                })?,
            ))
        })
        .transpose()?;

    // Exclude our own row: this function re-ingests an existing row whose canonical_id
    // is derived from the same parsed contents, so without this filter the row would
    // match itself and the `dup.modified_time >= modified_time` check would short-circuit
    // every re-ingest, making `reingest_stale` silently a no-op.
    let canonical_duplicate = sqlx::query!(
        "SELECT id, modified_time FROM games WHERE canonical_id = $1 AND id <> $2",
        canonical_id,
        file_id,
    )
    .fetch_optional(pool)
    .await?;

    let canonical_duplicate_id = if let Some(ref dup) = canonical_duplicate {
        if dup.modified_time >= modified_time {
            info!(
                "duplicate game detected: skipping {} (existing {} has same or newer modified time)",
                file_id, dup.id
            );
            return Ok(());
        }
        info!(
            "duplicate game detected: replacing {} with {} (newer modified time)",
            dup.id, file_id
        );
        Some(dup.id.clone())
    } else {
        None
    };

    commit_file(
        pool,
        tx_sem,
        PreparedInsert {
            file_id: file_id.to_string(),
            file_name: file_name.to_string(),
            modified_time,
            source_str: source_str.to_string(),
            file_exists,
            canonical_duplicate_id,
            date,
            game,
            canonical_id,
            fingerprint_json,
            game_data,
            home_stats,
        },
    )
    .await
}

fn is_retryable(e: &sqlx::Error) -> bool {
    if let sqlx::Error::Database(db_err) = e {
        let code = db_err.code();
        return code.as_deref() == Some("40001") // serialization_failure
            || code.as_deref() == Some("CR000"); // crdb retry
    }
    false
}

// Startup-only pass for the Drive source: lists all files in the folder, diffs
// against known DB IDs, and ingests any that were previously skipped (e.g. due
// to a 4XX download error that caused the run_ingest cursor to advance past them).
// For the Local source this is effectively a no-op — run_ingest already lists all
// local files every poll (the `since` parameter is ignored for Local).
async fn reconcile_missing(
    cfg: &Config,
    pool: Arc<PgPool>,
    source: Arc<FileSource>,
    tx_sem: Arc<tokio::sync::Semaphore>,
) -> anyhow::Result<usize> {
    let all_files = source.list_all_xlsx(&cfg.google_drive_folder_id).await?;

    let known_ids: HashSet<String> = sqlx::query!("SELECT id FROM games")
        .fetch_all(&*pool)
        .await?
        .into_iter()
        .map(|r| r.id)
        .collect();

    let mut missing: Vec<DriveFile> = all_files
        .into_iter()
        .filter(|f| !known_ids.contains(&f.id))
        .collect();

    if missing.is_empty() {
        return Ok(0);
    }

    let n = missing.len();
    info!("found {n} file(s) missing from database, reconciling");

    // oldest-first for readable log output; not load-bearing (no cursor here)
    missing.sort_unstable_by(|a, b| a.modified_time.cmp(&b.modified_time));

    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    let prep_sem = Arc::new(tokio::sync::Semaphore::new(cores));
    let cache_dir = cfg.game_data_dir.clone();

    let mut ordered: FuturesOrdered<_> = missing
        .into_iter()
        .map(|file| {
            let pool = pool.clone();
            let source = source.clone();
            let prep_sem = prep_sem.clone();
            let cache_dir = cache_dir.clone();
            tokio::spawn(async move {
                let _permit = prep_sem
                    .acquire_owned()
                    .await
                    .expect("prep semaphore closed");
                let name = file.name.clone();
                let result = prepare_file(&pool, &source, &file, cache_dir.as_deref()).await;
                (name, result)
            })
        })
        .collect();

    let mut ingested = 0;
    let mut i = 0usize;
    while let Some(join_result) = ordered.next().await {
        i += 1;
        let (name, prep_result) = match join_result {
            Ok(r) => r,
            Err(e) => {
                warn!("prepare task panicked ({i}/{n}): {e}");
                continue;
            }
        };
        match prep_result {
            Err(e) => warn!("skipping {name}: {e:#}"),
            Ok(None) => {}
            Ok(Some(prep)) => match commit_file(&pool, &tx_sem, prep).await {
                Ok(()) => {
                    info!("reconciled ({i}/{n}) {name}");
                    ingested += 1;
                }
                Err(e) => warn!("skipping {name}: {e:#}"),
            },
        }
    }
    Ok(ingested)
}

async fn reingest_stale(
    pool: Arc<PgPool>,
    source: Option<Arc<FileSource>>,
    tx_sem: Arc<tokio::sync::Semaphore>,
    cache_dir: Option<PathBuf>,
    cache_only: bool,
) -> anyhow::Result<usize> {
    let rows = sqlx::query!(
        "SELECT id, source, modified_time, canonical_id FROM games WHERE parser_version < $1",
        parse::PARSER_VERSION
    )
    .fetch_all(&*pool)
    .await?;

    if rows.is_empty() {
        return Ok(0);
    }

    let rows = if cache_only {
        // No ingest source to fall back to: only games whose raw bytes are
        // already in the on-disk cache can be re-parsed.
        let dir = match cache_dir.as_deref() {
            Some(dir) => dir,
            None => {
                warn!("cache-only stale re-ingest skipped: GAME_DATA_DIR is not set");
                return Ok(0);
            }
        };
        let (cached, missing): (Vec<_>, Vec<_>) = rows
            .into_iter()
            .partition(|row| data_cache::read_game_data(dir, &row.canonical_id).is_some());
        if !missing.is_empty() {
            info!(
                "skipping {} stale game(s) not present in the on-disk cache",
                missing.len()
            );
        }
        cached
    } else {
        let source = source
            .as_ref()
            .expect("ingest source required when cache_only is false");
        let source_str = source.source_str();
        let (rows, skipped): (Vec<_>, Vec<_>) =
            rows.into_iter().partition(|row| row.source == source_str);
        if !skipped.is_empty() {
            warn!(
                "skipping {} stale game(s) due to source mismatch (runtime={})",
                skipped.len(),
                source_str
            );
        }
        rows
    };

    if rows.is_empty() {
        return Ok(0);
    }

    info!(
        "re-ingesting {} game(s) with outdated parser version",
        rows.len()
    );

    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    let sem = Arc::new(tokio::sync::Semaphore::new(cores));
    let mut set = tokio::task::JoinSet::new();
    let total = rows.len();

    for (i, row) in rows.into_iter().enumerate() {
        let permit = sem.clone().acquire_owned().await?;
        let pool = pool.clone();
        let source = source.clone();
        let tx_sem = tx_sem.clone();
        let cache_dir = cache_dir.clone();
        set.spawn(async move {
            let _permit = permit;
            info!("re-ingesting {}/{}: {}", i + 1, total, row.id);
            let cached = cache_dir
                .as_deref()
                .and_then(|d| data_cache::read_game_data(d, &row.canonical_id));
            if cache_only && cached.is_none() {
                warn!("skipping {}: not present in the on-disk cache", row.id);
                return 0;
            }
            let used_cache = cached.is_some();
            let mut result = insert_parsed_file(
                &pool,
                source.as_deref(),
                &row.source,
                &row.id,
                &row.id,
                row.modified_time,
                &tx_sem,
                true,
                cached,
                cache_dir.as_deref(),
            )
            .await;
            if result.is_err() && used_cache && !cache_only {
                warn!(
                    "re-ingest of {} from cache failed; falling back to download",
                    row.id
                );
                result = insert_parsed_file(
                    &pool,
                    source.as_deref(),
                    &row.source,
                    &row.id,
                    &row.id,
                    row.modified_time,
                    &tx_sem,
                    true,
                    None,
                    cache_dir.as_deref(),
                )
                .await;
            }
            match result {
                Ok(()) => 1,
                Err(e) => {
                    warn!("re-ingest failed for {}: {e:#}", row.id);
                    0
                }
            }
        });
    }

    let mut count = 0;
    while let Some(result) = set.join_next().await {
        count += result?;
    }
    Ok(count)
}

/// Re-parses every game whose parser_version is behind PARSER_VERSION using
/// only the on-disk cache — the ingest source is never contacted. Runs at
/// startup regardless of INGEST_ENABLED so cached games are refreshed whenever
/// the parser changes.
pub async fn reingest_stale_from_cache(
    pool: Arc<PgPool>,
    cache_dir: Option<PathBuf>,
) -> anyhow::Result<usize> {
    let tx_sem = Arc::new(tokio::sync::Semaphore::new(1));
    reingest_stale(pool, None, tx_sem, cache_dir, true).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{GameData, TeamData, Venue};

    #[test]
    fn file_source_str_drive() {
        let sa = std::sync::Arc::new(drive_auth::ServiceAccount {
            client_email: "test@test.iam.gserviceaccount.com".into(),
            private_key: "-----BEGIN PRIVATE KEY-----\nfake\n-----END PRIVATE KEY-----\n".into(),
            token_uri: "https://oauth2.googleapis.com/token".into(),
        });
        let s = FileSource::Drive(drive::DriveClient::new(sa));
        assert_eq!(s.source_str(), "drive");
    }

    #[test]
    fn file_source_str_local() {
        let s = FileSource::Local(LocalSource::new(std::path::PathBuf::from("/tmp")));
        assert_eq!(s.source_str(), "file");
    }

    fn make_game(
        home_league: &str,
        home_team: &str,
        away_league: &str,
        away_team: &str,
    ) -> GameData {
        GameData {
            version: "2024".into(),
            venue: Venue {
                name: None,
                city: None,
                state: None,
            },
            tournament: None,
            host_league: None,
            home: TeamData {
                league: Some(home_league.into()),
                team: Some(home_team.into()),
                color: None,
                skaters: vec![],
            },
            away: TeamData {
                league: Some(away_league.into()),
                team: Some(away_team.into()),
                color: None,
                skaters: vec![],
            },
            periods: vec![],
            penalties: vec![],
            game_summary: None,
            home_score: 0,
            away_score: 0,
        }
    }

    #[test]
    fn build_fingerprint_happy_path() {
        let game = make_game("Home League", "Home Team", "Away League", "Away Team");
        let date = chrono::NaiveDate::from_ymd_opt(2026, 5, 2).unwrap();
        let fp = build_fingerprint(&game, Some(date)).unwrap();
        assert_eq!(fp.date, date);
        assert_eq!(fp.home_league, "Home League");
        assert_eq!(fp.home_team, "Home Team");
        assert_eq!(fp.away_league, "Away League");
        assert_eq!(fp.away_team, "Away Team");
        assert_eq!(fp.home_score, 0);
        assert_eq!(fp.away_score, 0);
    }

    #[test]
    fn build_fingerprint_missing_date_errors() {
        let game = make_game("A", "B", "C", "D");
        assert!(build_fingerprint(&game, None).is_err());
    }

    #[test]
    fn build_fingerprint_missing_field_errors() {
        let date = chrono::NaiveDate::from_ymd_opt(2026, 5, 2).unwrap();
        let mut game = make_game("A", "B", "C", "D");
        game.home.league = None;
        assert!(build_fingerprint(&game, Some(date)).is_err());
    }
}
