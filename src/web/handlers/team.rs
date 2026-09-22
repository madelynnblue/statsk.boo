use crate::web::{AppState, error::AppError};
use axum::extract::{Path, State};
use axum::response::Html;
use serde::Serialize;

#[derive(Serialize)]
struct GameRow {
    game_id: i64,
    date: String,
    opponent_id: i64,
    opponent_title: String,
    opponent_slug: Option<String>,
    our_score: i16,
    their_score: i16,
    result: String,
    /// Present when `game_matches` links a statsbook to this WFTDA game, so the
    /// row can offer the "StatsBook" link in addition to the WFTDA one.
    statsbook_drive_file_id: Option<String>,
}

#[derive(Serialize)]
struct Record {
    wins: usize,
    losses: usize,
    ties: usize,
}

pub async fn handle(
    State(state): State<AppState>,
    Path(wftda_team_id): Path<i64>,
) -> Result<Html<String>, AppError> {
    let team = sqlx::query!(
        r#"SELECT id, title, slug AS "slug?", region AS "region?"
             FROM wftda_teams
            WHERE id = $1"#,
        wftda_team_id,
    )
    .fetch_optional(&*state.pool)
    .await?
    .ok_or(AppError::NotFound)?;

    let rows = sqlx::query!(
        r#"SELECT g.id,
                  g.date,
                  CASE WHEN g.team_1_id = $1 THEN g.team_2_id ELSE g.team_1_id END AS "opponent_id!",
                  opp.title AS opponent_title,
                  opp.slug  AS "opponent_slug?",
                  CASE WHEN g.team_1_id = $1 THEN g.team_1_score ELSE g.team_2_score END AS "our_score!",
                  CASE WHEN g.team_1_id = $1 THEN g.team_2_score ELSE g.team_1_score END AS "their_score!",
                  (SELECT m.drive_file_id
                     FROM game_matches m
                    WHERE m.wftda_game_id = g.id
                    ORDER BY m.drive_file_id
                    LIMIT 1) AS "statsbook_drive_file_id?"
             FROM wftda_games g
             JOIN wftda_teams opp
               ON opp.id = CASE WHEN g.team_1_id = $1 THEN g.team_2_id ELSE g.team_1_id END
            WHERE g.team_1_id = $1 OR g.team_2_id = $1
            ORDER BY g.date DESC, g.id DESC"#,
        wftda_team_id,
    )
    .fetch_all(&*state.pool)
    .await?;

    let mut record = Record {
        wins: 0,
        losses: 0,
        ties: 0,
    };
    let mut games: Vec<GameRow> = Vec::with_capacity(rows.len());
    for row in rows {
        let result = match row.our_score.cmp(&row.their_score) {
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
        games.push(GameRow {
            game_id: row.id,
            date: row.date.to_string(),
            opponent_id: row.opponent_id,
            opponent_title: row.opponent_title,
            opponent_slug: row.opponent_slug,
            our_score: row.our_score,
            their_score: row.their_score,
            result: result.to_string(),
            statsbook_drive_file_id: row.statsbook_drive_file_id,
        });
    }

    let tmpl = state.env.get_template("team.html")?;
    let html = tmpl.render(minijinja::context! {
        team_id => team.id,
        title   => team.title,
        slug    => team.slug,
        region  => team.region,
        record,
        games,
    })?;
    Ok(Html(html))
}
