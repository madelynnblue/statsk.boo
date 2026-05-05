#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let cfg = wsb::config::Config::from_env()?;
    let pool = wsb::db::connect(&cfg.database_url).await?;
    wsb::db::migrate(&pool).await?;

    let cfg = std::sync::Arc::new(cfg);
    let pool = std::sync::Arc::new(pool);

    let ingest_cfg = cfg.clone();
    let ingest_pool = pool.clone();
    tokio::spawn(async move {
        wsb::ingest::ingest_loop(ingest_cfg, ingest_pool).await;
    });

    wsb::web::serve(cfg, pool).await
}
