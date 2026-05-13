use crate::web::{AppState, error::AppError};
use axum::extract::State;
use axum::response::Html;
use serde::Serialize;

pub async fn handle(State(state): State<AppState>) -> Result<Html<String>, AppError> {
    let rows = sqlx::query!(
        r#"SELECT g.canonical_id, g.date, g.home_score, g.away_score,
                  home.team as home_team, home.league as home_league,
                  away.team as away_team, away.league as away_league
           FROM (SELECT id, canonical_id, date, ingested_at, home_score, away_score
                 FROM games ORDER BY date DESC, ingested_at DESC LIMIT 25) g
           INNER LOOKUP JOIN game_sides home ON home.game_id = g.id AND home.side = 'home'
           INNER LOOKUP JOIN game_sides away ON away.game_id = g.id AND away.side = 'away'
           ORDER BY g.date DESC, g.ingested_at DESC"#,
    )
    .fetch_all(&*state.pool)
    .await?;

    let games: Vec<RecentGame> = rows
        .into_iter()
        .map(|r| RecentGame {
            game_id: r.canonical_id,
            date: r.date,
            home_score: r.home_score,
            away_score: r.away_score,
            home_team: r.home_team.unwrap_or_default(),
            home_league: r.home_league.unwrap_or_default(),
            away_team: r.away_team.unwrap_or_default(),
            away_league: r.away_league.unwrap_or_default(),
        })
        .collect();

    let tmpl = state.env.get_template("index.html")?;
    let html = tmpl.render(minijinja::context! { games })?;
    Ok(Html(html))
}

#[derive(Serialize)]
struct RecentGame {
    game_id: String,
    date: chrono::NaiveDate,
    home_score: i16,
    away_score: i16,
    home_team: String,
    home_league: String,
    away_team: String,
    away_league: String,
}
