use axum::extract::{Query, State};
use axum::response::Html;
use serde::{Deserialize, Serialize};
use crate::models::GameData;
use crate::web::{AppState, error::AppError};

#[derive(Deserialize)]
pub struct TeamParams {
    pub league: String,
    pub team: String,
}

#[derive(Serialize)]
struct GameRow {
    drive_file_id: String,
    date: String,
    side: String,
    our_score: i16,
    their_score: i16,
    opponent_team: String,
    opponent_league: String,
    result: String,
}

#[derive(Serialize)]
struct Record {
    wins: usize,
    losses: usize,
    ties: usize,
}

pub async fn handle(
    State(state): State<AppState>,
    Query(params): Query<TeamParams>,
) -> Result<Html<String>, AppError> {
    let rows = sqlx::query!(
        r#"SELECT drive_file_id, date, data::text as data_text FROM games
           WHERE data @> jsonb_build_object('home', jsonb_build_object('league', $1::text, 'team', $2::text))
              OR data @> jsonb_build_object('away', jsonb_build_object('league', $1::text, 'team', $2::text))
           ORDER BY date DESC"#,
        params.league,
        params.team,
    )
    .fetch_all(&*state.pool).await?;

    let mut game_rows: Vec<GameRow> = Vec::new();
    let mut record = Record { wins: 0, losses: 0, ties: 0 };

    for row in &rows {
        let game: GameData = serde_json::from_str(row.data_text.as_deref().unwrap_or_default())
            .map_err(anyhow::Error::from)?;

        let side = if game.home.league.as_deref() == Some(&params.league)
            && game.home.team.as_deref() == Some(&params.team)
        { "home" } else { "away" };

        let home_score = game.total_score("home");
        let away_score = game.total_score("away");

        let (our_score, their_score) = if side == "home" {
            (home_score, away_score)
        } else {
            (away_score, home_score)
        };

        let result = match our_score.cmp(&their_score) {
            std::cmp::Ordering::Greater => { record.wins += 1; "W" }
            std::cmp::Ordering::Less    => { record.losses += 1; "L" }
            std::cmp::Ordering::Equal   => { record.ties += 1; "T" }
        };

        let opponent = if side == "home" { &game.away } else { &game.home };

        game_rows.push(GameRow {
            drive_file_id: row.drive_file_id.clone(),
            date: row.date.map(|d| d.to_string()).unwrap_or_default(),
            side: side.to_string(),
            our_score,
            their_score,
            opponent_team: opponent.team.clone().unwrap_or_default(),
            opponent_league: opponent.league.clone().unwrap_or_default(),
            result: result.to_string(),
        });
    }

    let tmpl = state.env.get_template("team.html")?;
    let html = tmpl.render(minijinja::context! {
        league => params.league,
        team   => params.team,
        record => record,
        games  => game_rows,
    })?;
    Ok(Html(html))
}
