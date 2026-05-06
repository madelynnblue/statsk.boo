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

/// Downloads, parses, and inserts a game into the database.
/// The executor can be a `&PgPool` or a transaction (`&mut PgConnection`).
async fn insert_parsed_file<'e, E>(
    executor: E,
    client: &DriveClient,
    file_id: &str,
    file_name: &str,
) -> anyhow::Result<()>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
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

    let player_search = game.player_search_text();
    let team_search = game.team_search_text();
    let data = serde_json::to_value(&game)?;

    sqlx::query!(
        r#"INSERT INTO games
           (drive_file_id, date, data, player_search, team_search, parser_version)
           VALUES ($1, $2, $3, $4, $5, $6)
           ON CONFLICT (drive_file_id) DO NOTHING"#,
        file_id,
        date,
        &data,
        &player_search,
        &team_search,
        parse::PARSER_VERSION,
    )
    .execute(executor)
    .await?;

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
    let mut tx = pool.begin().await?;
    sqlx::query!("DELETE FROM games WHERE drive_file_id = $1", file_id)
        .execute(&mut *tx)
        .await?;
    insert_parsed_file(&mut *tx, client, file_id, file_id).await?;
    tx.commit().await?;
    Ok(())
}
