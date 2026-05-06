pub mod drive;
pub mod parse;

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

    fn list_xlsx_since(&self, since: &str) -> anyhow::Result<Vec<DriveFile>> {
        let since_dt = chrono::DateTime::parse_from_rfc3339(since)
            .with_context(|| format!("parsing since timestamp: {since}"))?;
        let mut files = Vec::new();
        visit_dir(&self.root, &self.root, &since_dt, &mut files)?;
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
    since: &chrono::DateTime<chrono::FixedOffset>,
    files: &mut Vec<DriveFile>,
) -> anyhow::Result<()> {
    for entry in std::fs::read_dir(dir).with_context(|| format!("reading dir {}", dir.display()))? {
        let entry = entry.with_context(|| format!("entry in {}", dir.display()))?;
        let path = entry.path();
        if path.is_dir() {
            visit_dir(root, &path, since, files)?;
        } else if path.extension().map_or(false, |e| e == "xlsx") {
            let df = DriveFile::from_local(&path, root)?;
            let mtime = chrono::DateTime::parse_from_rfc3339(&df.modified_time)
                .unwrap_or_else(|_| chrono::DateTime::UNIX_EPOCH.fixed_offset());
            if mtime > *since {
                files.push(df);
            }
        }
    }
    Ok(())
}

enum FileSource {
    Drive(DriveClient),
    Local(LocalSource),
}

impl FileSource {
    async fn list_xlsx_since(
        &self,
        folder_id: &str,
        since: &str,
    ) -> anyhow::Result<Vec<DriveFile>> {
        match self {
            FileSource::Drive(c) => c.list_xlsx_since(folder_id, since).await,
            FileSource::Local(s) => s.list_xlsx_since(since),
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
    let source = if let Some(ref dir) = cfg.ingest_dir {
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
    };

    match reingest_stale(&pool, &source).await {
        Ok(n) if n > 0 => info!("re-ingested {n} stale game(s)"),
        Err(e) => error!("re-ingest of stale games failed: {e:#}"),
        _ => {}
    }

    loop {
        if let Err(e) = run_ingest(&cfg, &pool, &source).await {
            error!("ingest run failed: {e:#}");
        }
        tokio::time::sleep(cfg.ingest_interval).await;
    }
}

async fn run_ingest(cfg: &Config, pool: &PgPool, source: &FileSource) -> anyhow::Result<()> {
    let last_ingest = last_ingest_at(pool)
        .await?
        .unwrap_or_else(|| chrono::Utc::now() - chrono::Duration::weeks(1));

    let jitter = chrono::Duration::from_std(cfg.ingest_jitter).unwrap_or_default();
    let since = (last_ingest - jitter).to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    info!("ingesting files since {since}");
    let files = source
        .list_xlsx_since(&cfg.google_drive_folder_id, &since)
        .await?;

    info!("found {} candidate file(s)", files.len());

    for file in files {
        info!("ingesting {}", file.name);
        match process_file(pool, source, &file).await {
            Ok(true) => info!("ingested {}", file.name),
            Ok(false) => info!("skipped {} (already present)", file.name),
            Err(e) => warn!("skipping {}: {e:#}", file.name),
        }
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
) -> anyhow::Result<bool> {
    let modified_time: DateTime<Utc> = file.modified_time.parse()?;

    // Skip if we already have this file at this (or a newer) modified time. This
    // saves re-downloading on every poll for files in the `since` window that
    // haven't actually changed.
    let existing = sqlx::query!(
        "SELECT modified_time FROM games WHERE drive_file_id = $1",
        file.id,
    )
    .fetch_optional(pool)
    .await?;
    if let Some(existing) = existing
        && existing.modified_time >= modified_time
    {
        return Ok(false);
    }

    insert_parsed_file(pool, source, &file.id, &file.name, modified_time).await?;
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
    file_id: &str,
    file_name: &str,
    modified_time: DateTime<Utc>,
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

    let mut tx = pool.begin().await?;

    // Remove the row we are about to replace (noop for new files; clears old data for re-ingest).
    sqlx::query!("DELETE FROM games WHERE drive_file_id = $1", file_id)
        .execute(&mut *tx)
        .await?;

    // Check for a duplicate by fingerprint. The self-delete above ensures we never match
    // the row we just removed, so this can only find a genuinely different file.
    let existing = sqlx::query!(
        r#"SELECT drive_file_id, modified_time FROM games WHERE fingerprint = $1 ORDER BY modified_time DESC LIMIT 1"#,
        &fingerprint_json,
    )
    .fetch_optional(&mut *tx)
    .await?;

    if let Some(ref existing) = existing {
        if existing.modified_time >= modified_time {
            info!(
                "duplicate game detected: skipping {} (existing {} has same or newer modified time)",
                file_id, existing.drive_file_id
            );
            // Roll back the self-delete above so the existing row we matched on
            // (and any sibling row with the same drive_file_id, if this is a
            // re-ingest path) is preserved.
            tx.rollback().await?;
            return Ok(());
        }
        info!(
            "duplicate game detected: replacing {} with {} (newer modified time)",
            existing.drive_file_id, file_id
        );
        sqlx::query!(
            "DELETE FROM games WHERE drive_file_id = $1",
            existing.drive_file_id
        )
        .execute(&mut *tx)
        .await?;
    }

    sqlx::query!(
        r#"INSERT INTO games
           (drive_file_id, date, parser_version, version, tournament, host_league,
            venue_name, venue_city, venue_state, periods, penalties,
            modified_time, fingerprint)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)"#,
        file_id,
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
        sqlx::query!(
            "INSERT INTO game_sides (drive_file_id, side, league, team, color) VALUES ($1, $2, $3, $4, $5)",
            file_id,
            side_key,
            side.league,
            side.team,
            side.color,
        )
        .execute(&mut *tx)
        .await?;

        for skater in &side.skaters {
            sqlx::query!(
                "INSERT INTO game_skaters (drive_file_id, side, number, name) VALUES ($1, $2, $3, $4)",
                file_id,
                side_key,
                skater.number,
                skater.name,
            )
            .execute(&mut *tx)
            .await?;
        }
    }

    if let Some(ref summary) = game.game_summary {
        let home_stats = serde_json::to_value(&crate::models::SideStats {
            players: summary.home_players.clone(),
            totals: summary.home_totals.clone(),
        })?;
        let away_stats = serde_json::to_value(&crate::models::SideStats {
            players: summary.away_players.clone(),
            totals: summary.away_totals.clone(),
        })?;
        sqlx::query!(
            "INSERT INTO game_summary (drive_file_id, side, stats) VALUES ($1, 'home', $2)",
            file_id,
            &home_stats,
        )
        .execute(&mut *tx)
        .await?;
        sqlx::query!(
            "INSERT INTO game_summary (drive_file_id, side, stats) VALUES ($1, 'away', $2)",
            file_id,
            &away_stats,
        )
        .execute(&mut *tx)
        .await?;
    }

    tx.commit().await?;
    Ok(())
}

async fn reingest_stale(pool: &PgPool, source: &FileSource) -> anyhow::Result<usize> {
    let rows = sqlx::query!(
        "SELECT drive_file_id, modified_time FROM games WHERE parser_version < $1",
        parse::PARSER_VERSION
    )
    .fetch_all(pool)
    .await?;

    if rows.is_empty() {
        return Ok(0);
    }

    info!(
        "re-ingesting {} game(s) with outdated parser version",
        rows.len()
    );

    let mut count = 0;
    for (i, row) in rows.iter().enumerate() {
        info!(
            "re-ingesting {}/{}: {}",
            i + 1,
            rows.len(),
            row.drive_file_id
        );
        match insert_parsed_file(
            pool,
            source,
            &row.drive_file_id,
            &row.drive_file_id,
            row.modified_time,
        )
        .await
        {
            Ok(()) => count += 1,
            Err(e) => warn!("re-ingest failed for {}: {e:#}", row.drive_file_id),
        }
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{GameData, TeamData, Venue};

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
