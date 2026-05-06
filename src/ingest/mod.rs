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
    loop {
        if let Err(e) = run_ingest(&cfg, &pool, &client).await {
            error!("ingest run failed: {e:#}");
        }
        tokio::time::sleep(cfg.ingest_interval).await;
    }
}

async fn game_count(pool: &PgPool) -> anyhow::Result<i64> {
    let row = sqlx::query!("SELECT COUNT(*) as count FROM games")
        .fetch_one(pool)
        .await?;
    Ok(row.count.unwrap_or(0))
}

async fn run_ingest(cfg: &Config, pool: &PgPool, client: &DriveClient) -> anyhow::Result<()> {
    let count = game_count(pool).await?;
    if count >= 10 {
        info!("database has {count} games, skipping ingest (dev mode)");
        return Ok(());
    }

    let last_ingest = last_ingest_at(pool).await?;

    let files = match last_ingest {
        None => {
            info!("first ingest run: listing all files");
            client.list_all_xlsx(&cfg.google_drive_folder_id).await?
        }
        Some(ts) => {
            let jitter = chrono::Duration::from_std(cfg.ingest_jitter).unwrap_or_default();
            let since = (ts - jitter).to_rfc3339();
            info!("incremental ingest since {since}");
            client
                .list_xlsx_since(&cfg.google_drive_folder_id, &since)
                .await?
        }
    };

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

    let bytes = client.download_file(file_id).await?;
    let (game, date) = parse::parse_statsbook_with_date(&bytes)
        .map_err(|e| anyhow::anyhow!("parse error in {file_name}: {e:#}"))?;

    let player_search = game.player_search_text();
    let team_search = game.team_search_text();
    let data = serde_json::to_value(&game)?;

    sqlx::query!(
        r#"INSERT INTO games
           (drive_file_id, date, data, player_search, team_search)
           VALUES ($1, $2, $3, $4, $5)
           ON CONFLICT (drive_file_id) DO NOTHING"#,
        file_id,
        date,
        &data,
        &player_search,
        &team_search,
    )
    .execute(pool)
    .await?;

    Ok(true)
}
