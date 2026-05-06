use crate::models::GameData;
use crate::web::{AppState, error::AppError};
use axum::extract::State;
use axum::response::Html;

pub async fn handle(State(state): State<AppState>) -> Result<Html<String>, AppError> {
    let rows = sqlx::query!(
        r#"SELECT drive_file_id, date, data::text as data_text,
           data->'home'->>'team' as home_team, data->'home'->>'league' as home_league,
           data->'away'->>'team' as away_team, data->'away'->>'league' as away_league
           FROM games ORDER BY date DESC NULLS LAST, ingested_at DESC LIMIT 10"#,
    )
    .fetch_all(&*state.pool)
    .await?;

    let mut games: Vec<RecentGame> = Vec::new();
    for r in &rows {
        let game: GameData = serde_json::from_str(r.data_text.as_deref().unwrap_or_default())
            .map_err(anyhow::Error::from)?;
        games.push(RecentGame {
            drive_file_id: r.drive_file_id.clone(),
            date: r.date,
            home_score: game.total_score("home"),
            away_score: game.total_score("away"),
            home_team: r.home_team.clone().unwrap_or_default(),
            home_league: r.home_league.clone().unwrap_or_default(),
            away_team: r.away_team.clone().unwrap_or_default(),
            away_league: r.away_league.clone().unwrap_or_default(),
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
    home_league: String,
    away_team: String,
    away_league: String,
}
