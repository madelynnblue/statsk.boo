use axum::extract::State;
use axum::response::Html;
use crate::models::GameData;
use crate::web::{AppState, error::AppError};
use sqlx::Row;

pub async fn handle(State(state): State<AppState>) -> Result<Html<String>, AppError> {
    let rows = sqlx::query(
        "SELECT drive_file_id, date, data::text as data_text, \
         data->'home'->>'team' as home_team, data->'away'->>'team' as away_team \
         FROM games ORDER BY date DESC NULLS LAST, ingested_at DESC LIMIT 10",
    )
    .fetch_all(&*state.pool).await?;

    let mut games: Vec<RecentGame> = Vec::new();
    for r in &rows {
        let data_text: Option<String> = r.try_get("data_text")?;
        let game: GameData = serde_json::from_str(&data_text.unwrap_or_default())
            .map_err(anyhow::Error::from)?;
        games.push(RecentGame {
            drive_file_id: r.try_get("drive_file_id").unwrap_or_default(),
            date: r.try_get::<Option<chrono::NaiveDate>, _>("date").ok().flatten(),
            home_score: game.total_score("home"),
            away_score: game.total_score("away"),
            home_team: r.try_get("home_team").unwrap_or_default(),
            away_team: r.try_get("away_team").unwrap_or_default(),
        });
    }

    let tmpl = state.env.get_template("index.html")?;
    let html = tmpl.render(minijinja::context! { games })?;
    Ok(Html(html))
}

#[derive(serde::Serialize)]
struct RecentGame {
    drive_file_id: String,
    date: Option<chrono::NaiveDate>,
    home_score: i16,
    away_score: i16,
    home_team: String,
    away_team: String,
}
