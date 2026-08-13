#[tokio::main]
async fn main() -> anyhow::Result<()> {
    use std::io::IsTerminal;
    tracing_subscriber::fmt()
        .with_ansi(std::io::stderr().is_terminal())
        .with_env_filter(
            tracing_subscriber::EnvFilter::builder()
                .with_default_directive(tracing_subscriber::filter::LevelFilter::INFO.into())
                .from_env_lossy(),
        )
        .init();
    let cfg = wsb::config::Config::from_env()?;
    let pool = wsb::db::connect(&cfg.database_url).await?;
    wsb::db::migrate(&pool).await?;

    let cfg = std::sync::Arc::new(cfg);
    let pool = std::sync::Arc::new(pool);
    let cache = std::sync::Arc::new(wsb::cache::Cache::new());

    let ingest_cfg = cfg.clone();
    let ingest_pool = pool.clone();
    let ingest_cache = cache.clone();
    // Always re-parse stale games from the on-disk cache on startup (no ingest
    // source access), then start the full ingest loop only if enabled.
    tokio::spawn(async move {
        match wsb::ingest::reingest_stale_from_cache(
            ingest_pool.clone(),
            ingest_cfg.game_data_dir.clone(),
        )
        .await
        {
            Ok(n) if n > 0 => {
                tracing::info!("re-ingested {n} stale game(s) from cache");
                ingest_cache.clear();
            }
            Ok(_) => {}
            Err(e) => tracing::error!("cache-only re-ingest of stale games failed: {e:#}"),
        }

        if ingest_cfg.ingest_enabled {
            wsb::ingest::ingest_loop(ingest_cfg, ingest_pool, ingest_cache).await;
        } else {
            tracing::info!("ingest loop disabled (INGEST_ENABLED=false)");
        }
    });

    wsb::web::serve(cfg, pool, cache).await
}
