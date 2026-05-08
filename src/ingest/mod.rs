pub mod drive;
pub mod parse;

use crate::canon::{canonicalize_league, canonicalize_team};
use crate::config::Config;
use crate::models::{GameData, periods_score};
use anyhow::Context;
use chrono::{DateTime, NaiveDate, Utc};
use drive::DriveClient;
use drive::DriveFile;
use serde::Serialize;
use sqlx::PgPool;
use std::path::PathBuf;
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

    async fn read_file(&self, file_id: &str) -> anyhow::Result<Vec<u8>> {
        match self {
            FileSource::Drive(c) => c.download_file(file_id).await,
            FileSource::Local(s) => s.read_file(file_id),
        }
    }
}

pub async fn ingest_loop(cfg: Arc<Config>, pool: Arc<PgPool>) {
    info!("ingest loop starting");
    let source = Arc::new(if let Some(ref dir) = cfg.ingest_dir {
        let root = PathBuf::from(dir);
        if !root.is_dir() {
            panic!("INGEST_DIR is not a directory: {dir}");
        }
        FileSource::Local(LocalSource::new(root))
    } else {
        FileSource::Drive(DriveClient::new(
            cfg.google_api_key
                .clone()
                .expect("GOOGLE_API_KEY required when INGEST_DIR not set"),
        ))
    });

    // One transaction at a time: the fingerprint GIN scan reads the whole table snapshot,
    // which concurrent writes invalidate, causing RETRY_SERIALIZABLE on commit.
    // Downloads and parsing happen in parallel; only the DB transaction is serialized.
    let tx_sem = Arc::new(tokio::sync::Semaphore::new(1));

    match reingest_stale(pool.clone(), source.clone(), tx_sem.clone()).await {
        Ok(n) if n > 0 => info!("re-ingested {n} stale game(s)"),
        Err(e) => error!("re-ingest of stale games failed: {e:#}"),
        _ => {}
    }

    loop {
        if let Err(e) = run_ingest(&cfg, pool.clone(), source.clone(), tx_sem.clone()).await {
            error!("ingest run failed: {e:#}");
        }
        tokio::time::sleep(cfg.ingest_interval).await;
    }
}

async fn run_ingest(
    cfg: &Config,
    pool: Arc<PgPool>,
    source: Arc<FileSource>,
    tx_sem: Arc<tokio::sync::Semaphore>,
) -> anyhow::Result<()> {
    let last_ingest = last_ingest_at(&pool)
        .await?
        .unwrap_or(chrono::DateTime::UNIX_EPOCH);

    let jitter = chrono::Duration::from_std(cfg.ingest_jitter).unwrap_or_default();
    let since = (last_ingest - jitter).to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    info!("ingesting files since {since}");
    let files = source
        .list_xlsx_since(&cfg.google_drive_folder_id, &since)
        .await?;

    let n = files.len();
    info!("found {n} candidate file(s)");

    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    let process_sem = Arc::new(tokio::sync::Semaphore::new(cores));
    let mut set = tokio::task::JoinSet::new();

    for file in files {
        let permit = process_sem.clone().acquire_owned().await?;
        let pool = pool.clone();
        let source = source.clone();
        let tx_sem = tx_sem.clone();
        set.spawn(async move {
            let _permit = permit;
            let name = file.name.clone();
            let res = process_file(&pool, &source, &file, &tx_sem).await;
            (name, res)
        });
    }

    let mut skipped = 0;
    while let Some(result) = set.join_next().await {
        let (name, result) = result?;
        match result {
            Ok(true) => info!("ingested {name}"),
            Ok(false) => skipped += 1,
            Err(e) => warn!("skipping {name}: {e:#}"),
        }
    }
    if skipped > 0 {
        info!("skipped {skipped} (already present)");
    }

    Ok(())
}

async fn last_ingest_at(pool: &PgPool) -> anyhow::Result<Option<chrono::DateTime<chrono::Utc>>> {
    let row = sqlx::query!("SELECT MAX(ingested_at) as ts FROM games")
        .fetch_one(pool)
        .await?;
    Ok(row.ts)
}

async fn process_file(
    pool: &PgPool,
    source: &FileSource,
    file: &DriveFile,
    tx_sem: &tokio::sync::Semaphore,
) -> anyhow::Result<bool> {
    let modified_time: DateTime<Utc> = file.modified_time.parse()?;

    // Skip if we already have this file at this (or a newer) modified time. This
    // saves re-downloading on every poll for files in the `since` window that
    // haven't actually changed.
    let existing = sqlx::query!("SELECT modified_time FROM games WHERE id = $1", file.id,)
        .fetch_optional(pool)
        .await?;
    if let Some(existing) = existing
        && existing.modified_time >= modified_time
    {
        return Ok(false);
    }

    insert_parsed_file(pool, source, &file.id, &file.name, modified_time, tx_sem).await?;
    Ok(true)
}

#[derive(Debug, PartialEq, Eq, Serialize)]
struct GameFingerprint {
    date: NaiveDate,
    home_league: String,
    home_team: String,
    away_league: String,
    away_team: String,
    home_score: i16,
    away_score: i16,
}

fn build_fingerprint(game: &GameData, date: Option<NaiveDate>) -> anyhow::Result<GameFingerprint> {
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
        home_score: periods_score(&game.periods, "home"),
        away_score: periods_score(&game.periods, "away"),
    })
}

async fn insert_parsed_file(
    pool: &PgPool,
    source: &FileSource,
    file_id: &str, // used as both the download key and games.id PK
    file_name: &str,
    modified_time: DateTime<Utc>,
    tx_sem: &tokio::sync::Semaphore,
) -> anyhow::Result<()> {
    let bytes = source.read_file(file_id).await?;
    let (game, date) = parse::parse_statsbook_with_date(&bytes)
        .map_err(|e| anyhow::anyhow!("parse error in {file_name}: {e:#}"))?;

    if let Some(ref d) = date {
        let tomorrow = chrono::Utc::now().date_naive() + chrono::Duration::days(1);
        if *d > tomorrow {
            tracing::info!("skipping {file_name}: date {d} is more than 1 day in the future");
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
    let fingerprint_json = serde_json::to_value(&fingerprint)?;

    let periods = serde_json::to_value(&game.periods)?;
    let penalties = serde_json::to_value(&game.penalties)?;
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

    let _tx_permit = tx_sem.acquire().await?;
    let mut attempt = 0;
    loop {
        let mut tx = pool.begin().await?;

        // Remove the row we are about to replace (noop for new files; clears old data for re-ingest).
        sqlx::query!("DELETE FROM games WHERE id = $1", file_id)
            .execute(&mut *tx)
            .await?;

        // Check for a duplicate by fingerprint. The self-delete above ensures we never match
        // the row we just removed, so this can only find a genuinely different file.
        let existing = sqlx::query!(
            r#"SELECT id, modified_time FROM games WHERE fingerprint = $1 ORDER BY modified_time DESC LIMIT 1"#,
            &fingerprint_json,
        )
        .fetch_optional(&mut *tx)
        .await?;

        if let Some(ref existing) = existing {
            if existing.modified_time >= modified_time {
                info!(
                    "duplicate game detected: skipping {} (existing {} has same or newer modified time)",
                    file_id, existing.id
                );
                tx.rollback().await?;
                return Ok(());
            }
            info!(
                "duplicate game detected: replacing {} with {} (newer modified time)",
                existing.id, file_id
            );
            sqlx::query!("DELETE FROM games WHERE id = $1", existing.id)
                .execute(&mut *tx)
                .await?;
        }

        sqlx::query!(
            r#"INSERT INTO games
               (id, source, date, parser_version, version, tournament, host_league,
                venue_name, venue_city, venue_state, periods, penalties,
                modified_time, fingerprint)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)"#,
            file_id,
            source.source_str(),
            date,
            parse::PARSER_VERSION,
            game.version,
            game.tournament,
            game.host_league,
            game.venue.name,
            game.venue.city,
            game.venue.state,
            &periods,
            &penalties,
            modified_time,
            &fingerprint_json,
        )
        .execute(&mut *tx)
        .await?;

        for (side_key, side) in [("home", &game.home), ("away", &game.away)] {
            let league_canonical = canonicalize_league(side.league.as_deref().unwrap_or(""));
            let team_canonical =
                canonicalize_team(side.league.as_deref(), side.team.as_deref().unwrap_or(""));
            sqlx::query!(
                "INSERT INTO game_sides (game_id, side, league, team, color, league_canonical, team_canonical) VALUES ($1, $2, $3, $4, $5, $6, $7)",
                file_id,
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
                sqlx::query!(
                    "INSERT INTO game_skaters (game_id, side, number, name) VALUES ($1, $2, $3, $4)",
                    file_id,
                    side_key,
                    skater.number,
                    skater.name,
                )
                .execute(&mut *tx)
                .await?;
            }
        }

        if let Some((ref home_stats, ref away_stats)) = home_stats {
            sqlx::query!(
                "INSERT INTO game_summary (game_id, side, stats) VALUES ($1, 'home', $2)",
                file_id,
                home_stats,
            )
            .execute(&mut *tx)
            .await?;
            sqlx::query!(
                "INSERT INTO game_summary (game_id, side, stats) VALUES ($1, 'away', $2)",
                file_id,
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
                        "retryable transaction error for {file_name}, attempt {attempt}/5, waiting {delay:?}: {e}"
                    );
                    tokio::time::sleep(delay).await;
                    continue;
                }
                return Err(e.into());
            }
        }
    }
}

fn is_retryable(e: &sqlx::Error) -> bool {
    if let sqlx::Error::Database(db_err) = e {
        let code = db_err.code();
        return code.as_deref() == Some("40001") // serialization_failure
            || code.as_deref() == Some("CR000"); // crdb retry
    }
    false
}

async fn reingest_stale(
    pool: Arc<PgPool>,
    source: Arc<FileSource>,
    tx_sem: Arc<tokio::sync::Semaphore>,
) -> anyhow::Result<usize> {
    let rows = sqlx::query!(
        "SELECT id, source, modified_time FROM games WHERE parser_version < $1",
        parse::PARSER_VERSION
    )
    .fetch_all(&*pool)
    .await?;

    if rows.is_empty() {
        return Ok(0);
    }

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
        set.spawn(async move {
            let _permit = permit;
            info!("re-ingesting {}/{}: {}", i + 1, total, row.id);
            match insert_parsed_file(&pool, &source, &row.id, &row.id, row.modified_time, &tx_sem)
                .await
            {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{GameData, TeamData, Venue};

    #[test]
    fn file_source_str_drive() {
        let s = FileSource::Drive(drive::DriveClient::new("fake".into()));
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
