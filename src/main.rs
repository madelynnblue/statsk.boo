#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // One-off maintenance subcommands run before config/DB/migration/server
    // setup, so `statskboo rekey-cache ...` needs neither `DATABASE_URL` nor a
    // connection to the new database (it only reads the legacy DB and rewrites
    // files in the cache directory). Required entry point for prod, where the
    // image ships only this binary.
    let mut args = std::env::args();
    let _program = args.next();
    if let Some(subcommand) = args.next() {
        return match subcommand.as_str() {
            "rekey-cache" => statskboo::rekey_cache::run_cli(args).await,
            "-h" | "--help" => {
                println!("{}", statskboo::rekey_cache::USAGE);
                Ok(())
            }
            other => Err(anyhow::anyhow!(
                "unknown subcommand {other:?}; the only subcommand is `rekey-cache`\n\n{}",
                statskboo::rekey_cache::USAGE
            )),
        };
    }

    use std::io::IsTerminal;
    tracing_subscriber::fmt()
        .with_ansi(std::io::stderr().is_terminal())
        .with_env_filter(
            tracing_subscriber::EnvFilter::builder()
                .with_default_directive(tracing_subscriber::filter::LevelFilter::INFO.into())
                .from_env_lossy(),
        )
        .init();
    let cfg = statskboo::config::Config::from_env()?;
    let pool = statskboo::db::connect(&cfg.database_url).await?;
    statskboo::db::migrate(&pool).await?;

    let cfg = std::sync::Arc::new(cfg);
    let pool = std::sync::Arc::new(pool);
    let cache = std::sync::Arc::new(statskboo::cache::Cache::new());

    if cfg.wftda_sync_enabled {
        // WFTDA reference sync (stages 1-2) and the Drive statsbook index
        // (stage 3), parse (stage 4), and resolve (stages 5-6). The Drive loop
        // no-ops with a warning without a service account.
        let wftda_cfg = cfg.clone();
        let wftda_pool = pool.clone();
        let wftda_cache = cache.clone();
        tokio::spawn(async move {
            statskboo::ingest::wftda_sync_loop(wftda_cfg, wftda_pool, wftda_cache).await;
        });

        let drive_cfg = cfg.clone();
        let drive_pool = pool.clone();
        let drive_cache = cache.clone();
        tokio::spawn(async move {
            statskboo::ingest::drive_index_loop(drive_cfg, drive_pool, drive_cache).await;
        });
    } else {
        tracing::info!("WFTDA sync and Drive index disabled (WFTDA_SYNC_ENABLED=false)");
    }

    statskboo::web::serve(cfg, pool, cache).await
}
