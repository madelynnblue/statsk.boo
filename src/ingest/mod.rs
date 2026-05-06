pub mod drive;
pub mod parse;

use crate::config::Config;
use drive::DriveClient;
use sqlx::PgPool;
use std::sync::Arc;
use tracing::{error, info, warn};

pub async fn ingest_loop(cfg: Arc<Config>, pool: Arc<PgPool>) {
    info!("ingest loop starting");
    let client = DriveClient::new(cfg.google_api_key.clone());

    match reingest_stale(&pool, &client).await {
        Ok(n) if n > 0 => info!("re-ingested {n} stale game(s)"),
        Err(e) => error!("re-ingest of stale games failed: {e:#}"),
        _ => {}
    }

    loop {
        if let Err(e) = run_ingest(&cfg, &pool, &client).await {
            error!("ingest run failed: {e:#}");
        }
        tokio::time::sleep(cfg.ingest_interval).await;
    }
}

async fn run_ingest(cfg: &Config, pool: &PgPool, client: &DriveClient) -> anyhow::Result<()> {
    let last_ingest = last_ingest_at(pool)
        .await?
        .unwrap_or_else(|| chrono::Utc::now() - chrono::Duration::weeks(1));

    let jitter = chrono::Duration::from_std(cfg.ingest_jitter).unwrap_or_default();
    let since = (last_ingest - jitter).to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    info!("ingesting files since {since}");
    let files = client
        .list_xlsx_since(&cfg.google_drive_folder_id, &since)
        .await?;

    info!("found {} candidate file(s)", files.len());

    for file in files {
        info!("ingesting {}", file.name);
        match process_file(pool, client, &file.id, &file.name).await {
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
    client: &DriveClient,
    file_id: &str,
    file_name: &str,
) -> anyhow::Result<bool> {
    let row = sqlx::query!(
        "SELECT COUNT(*) as count FROM games WHERE drive_file_id = $1",
        file_id
    )
    .fetch_one(pool)
    .await?;
    if row.count.unwrap_or(0) > 0 {
        return Ok(false);
    }

    insert_parsed_file(pool, client, file_id, file_name).await?;
    Ok(true)
}

async fn insert_parsed_file(
    pool: &PgPool,
    client: &DriveClient,
    file_id: &str,
    file_name: &str,
) -> anyhow::Result<()> {
    let bytes = client.download_file(file_id).await?;
    let (game, date) = parse::parse_statsbook_with_date(&bytes)
        .map_err(|e| anyhow::anyhow!("parse error in {file_name}: {e:#}"))?;

    if let Some(ref d) = date {
        let tomorrow = chrono::Utc::now().date_naive() + chrono::Duration::days(1);
        if *d > tomorrow {
            tracing::info!("skipping {file_name}: date {d} is more than 1 day in the future");
            return Ok(());
        }
    }

    let periods = serde_json::to_value(&game.periods)?;
    let penalties = serde_json::to_value(&game.penalties)?;

    let mut tx = pool.begin().await?;

    // Clean up old child rows (for re-ingest), then insert game row.
    sqlx::query!("DELETE FROM game_summary WHERE drive_file_id = $1", file_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query!("DELETE FROM game_skaters WHERE drive_file_id = $1", file_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query!("DELETE FROM game_sides WHERE drive_file_id = $1", file_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query!("DELETE FROM games WHERE drive_file_id = $1", file_id)
        .execute(&mut *tx)
        .await?;

    sqlx::query!(
        r#"INSERT INTO games
           (drive_file_id, date, parser_version, version, tournament, host_league,
            venue_name, venue_city, venue_state, periods, penalties)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)"#,
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
    )
    .execute(&mut *tx)
    .await?;

    // Insert sides.
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

        // Insert skaters for this side.
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

    // Insert game summary if present.
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

async fn reingest_stale(pool: &PgPool, client: &DriveClient) -> anyhow::Result<usize> {
    let rows = sqlx::query!(
        "SELECT drive_file_id FROM games WHERE parser_version < $1",
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
        match reingest_file(pool, client, &row.drive_file_id).await {
            Ok(()) => count += 1,
            Err(e) => warn!("re-ingest failed for {}: {e:#}", row.drive_file_id),
        }
    }
    Ok(count)
}

async fn reingest_file(pool: &PgPool, client: &DriveClient, file_id: &str) -> anyhow::Result<()> {
    insert_parsed_file(pool, client, file_id, file_id).await
}
