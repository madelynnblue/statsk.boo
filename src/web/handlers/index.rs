use crate::web::{AppState, error::AppError};
use axum::extract::State;
use axum::response::Html;
use serde::Serialize;

pub async fn handle(State(state): State<AppState>) -> Result<Html<String>, AppError> {
    let rows = sqlx::query!(
        r#"SELECT g.id, g.date, g.team_1_score, g.team_2_score,
                  t1.id AS home_id, t1.title AS home_title,
                  t2.id AS away_id, t2.title AS away_title
             FROM wftda_games g
             JOIN wftda_teams t1 ON t1.id = g.team_1_id
             JOIN wftda_teams t2 ON t2.id = g.team_2_id
            ORDER BY g.date DESC, g.id DESC
            LIMIT 25"#,
    )
    .fetch_all(&*state.pool)
    .await?;

    let games: Vec<RecentGame> = rows
        .into_iter()
        .map(|r| RecentGame {
            game_id: r.id,
            date: r.date,
            home_score: r.team_1_score,
            away_score: r.team_2_score,
            home_id: r.home_id,
            home_title: r.home_title,
            away_id: r.away_id,
            away_title: r.away_title,
        })
        .collect();

    let tmpl = state.env.get_template("index.html")?;
    let html = tmpl.render(minijinja::context! { games })?;
    Ok(Html(html))
}

#[derive(Serialize)]
struct RecentGame {
    game_id: i64,
    date: chrono::NaiveDate,
    home_score: i16,
    away_score: i16,
    home_id: i64,
    home_title: String,
    away_id: i64,
    away_title: String,
}
