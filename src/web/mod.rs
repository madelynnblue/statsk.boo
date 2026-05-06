pub mod error;
pub mod handlers;

use crate::config::Config;
use axum::{Router, routing::get};
use minijinja::Environment;
use sqlx::PgPool;
use std::sync::Arc;

#[derive(Clone)]
pub struct AppState {
    pub pool: Arc<PgPool>,
    pub env: Arc<Environment<'static>>,
}

pub async fn serve(cfg: Arc<Config>, pool: Arc<PgPool>) -> anyhow::Result<()> {
    let env = Arc::new(build_template_env());
    let state = AppState { pool, env };

    let app = Router::new()
        .route("/", get(handlers::index::handle))
        .route("/search", get(handlers::search::handle))
        .route("/player", get(handlers::player::handle))
        .route("/team", get(handlers::team::handle))
        .route("/game/{drive_file_id}", get(handlers::game::handle))
        .with_state(state);

    let addr: std::net::SocketAddr = cfg.bind_addr.parse()?;
    tracing::info!("listening on {addr}");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

fn build_template_env() -> Environment<'static> {
    let mut env = Environment::new();
    env.add_template("base.html", include_str!("../../templates/base.html"))
        .unwrap();
    env.add_template("index.html", include_str!("../../templates/index.html"))
        .unwrap();
    env.add_template("search.html", include_str!("../../templates/search.html"))
        .unwrap();
    env.add_template("player.html", include_str!("../../templates/player.html"))
        .unwrap();
    env.add_template("team.html", include_str!("../../templates/team.html"))
        .unwrap();
    env.add_template("game.html", include_str!("../../templates/game.html"))
        .unwrap();
    env
}
