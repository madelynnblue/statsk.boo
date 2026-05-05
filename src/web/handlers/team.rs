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
    use sqlx::Row;

    let rows = sqlx::query(
        r#"SELECT drive_file_id, date, home_score, away_score, data::text as data_text FROM games
           WHERE data @> jsonb_build_object('home', jsonb_build_object('league', $1::text, 'team', $2::text))
              OR data @> jsonb_build_object('away', jsonb_build_object('league', $1::text, 'team', $2::text))
           ORDER BY date DESC"#,
    )
    .bind(&params.league)
    .bind(&params.team)
    .fetch_all(&*state.pool).await?;

    let mut game_rows: Vec<GameRow> = Vec::new();
    let mut record = Record { wins: 0, losses: 0, ties: 0 };

    for row in &rows {
        let data_text: Option<String> = row.try_get("data_text")?;
        let game: GameData = serde_json::from_str(&data_text.unwrap_or_default())
            .map_err(anyhow::Error::from)?;

        let side = if game.home.league.as_deref() == Some(&params.league)
            && game.home.team.as_deref() == Some(&params.team)
        { "home" } else { "away" };

        let home_score: i32 = row.try_get("home_score")?;
        let away_score: i32 = row.try_get("away_score")?;

        let (our_score, their_score) = if side == "home" {
            (home_score as i16, away_score as i16)
        } else {
            (away_score as i16, home_score as i16)
        };

        let result = match our_score.cmp(&their_score) {
            std::cmp::Ordering::Greater => { record.wins += 1; "W" }
            std::cmp::Ordering::Less    => { record.losses += 1; "L" }
            std::cmp::Ordering::Equal   => { record.ties += 1; "T" }
        };

        let opponent = if side == "home" { &game.away } else { &game.home };
        let drive_file_id: String = row.try_get("drive_file_id")?;
        let date: Option<chrono::NaiveDate> = row.try_get("date")?;

        game_rows.push(GameRow {
            drive_file_id,
            date: date.map(|d| d.to_string()).unwrap_or_default(),
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
