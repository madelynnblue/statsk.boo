use crate::models::{Period, periods_score};
use crate::web::{AppState, error::AppError};
use axum::extract::{Query, State};
use axum::response::Html;
use serde::{Deserialize, Serialize};

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
        r#"SELECT g.drive_file_id, g.date, g.periods,
                  team_side.side as "side!: String",
                  opp.league as opp_league, opp.team as opp_team
           FROM games g
           JOIN game_sides team_side ON team_side.drive_file_id = g.drive_file_id
           JOIN game_sides opp ON opp.drive_file_id = g.drive_file_id AND opp.side != team_side.side
           WHERE team_side.league = $1 AND team_side.team = $2
           ORDER BY g.date DESC"#,
        params.league,
        params.team,
    )
    .fetch_all(&*state.pool)
    .await?;

    let mut game_rows: Vec<GameRow> = Vec::new();
    let mut record = Record {
        wins: 0,
        losses: 0,
        ties: 0,
    };

    for row in &rows {
        let periods: Vec<Period> =
            serde_json::from_value(row.periods.clone()).map_err(anyhow::Error::from)?;
        let side = &row.side;
        let home_score = periods_score(&periods, "home");
        let away_score = periods_score(&periods, "away");
        let (our_score, their_score) = if side == "home" {
            (home_score, away_score)
        } else {
            (away_score, home_score)
        };

        let result = match our_score.cmp(&their_score) {
            std::cmp::Ordering::Greater => {
                record.wins += 1;
                "W"
            }
            std::cmp::Ordering::Less => {
                record.losses += 1;
                "L"
            }
            std::cmp::Ordering::Equal => {
                record.ties += 1;
                "T"
            }
        };

        game_rows.push(GameRow {
            drive_file_id: row.drive_file_id.clone(),
            date: row.date.to_string(),
            side: side.clone(),
            our_score,
            their_score,
            opponent_team: row.opp_team.clone().unwrap_or_default(),
            opponent_league: row.opp_league.clone().unwrap_or_default(),
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
