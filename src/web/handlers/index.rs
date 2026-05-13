use crate::models::{Period, periods_score};
use crate::web::{AppState, error::AppError};
use axum::extract::State;
use axum::response::Html;
use serde::Serialize;

pub async fn handle(State(state): State<AppState>) -> Result<Html<String>, AppError> {
    let rows = sqlx::query!(
        r#"SELECT g.canonical_id, g.date, g.periods,
                  home.team as home_team, home.league as home_league,
                  away.team as away_team, away.league as away_league
           FROM games g
           JOIN game_sides home ON home.game_id = g.id AND home.side = 'home'
           JOIN game_sides away ON away.game_id = g.id AND away.side = 'away'
           ORDER BY g.date DESC, g.ingested_at DESC
           LIMIT 10"#,
    )
    .fetch_all(&*state.pool)
    .await?;

    let mut games: Vec<RecentGame> = Vec::new();
    for r in &rows {
        let periods: Vec<Period> =
            serde_json::from_value(r.periods.clone()).map_err(anyhow::Error::from)?;
        let home_score = periods_score(&periods, "home");
        let away_score = periods_score(&periods, "away");
        games.push(RecentGame {
            game_id: r.canonical_id.clone(),
            date: r.date,
            home_score,
            away_score,
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
