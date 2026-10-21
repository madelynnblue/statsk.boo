use crate::web::handlers::leagues::derive_league;
use crate::web::{AppState, error::AppError};
use axum::extract::{Path, State};
use axum::response::{Html, IntoResponse, Redirect, Response};
use serde::Serialize;

#[derive(Serialize)]
struct GameRow {
    game_id: i64,
    date: String,
    opponent_title: String,
    /// WFTDA slug, when the team has one (`None` for stub teams).
    opponent_slug: Option<String>,
    /// Site path segment: the slug, or the numeric id as a fallback.
    opponent_path: String,
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
    Path(team_ident): Path<String>,
) -> Result<Response, AppError> {
    let numeric_id = team_ident.parse::<i64>().ok();
    let team = sqlx::query!(
        r#"SELECT id, title, slug AS "slug?", region AS "region?"
             FROM wftda_teams
            WHERE slug = $1 OR id = $2"#,
        &team_ident,
        numeric_id,
    )
    .fetch_optional(&*state.pool)
    .await?
    .ok_or(AppError::NotFound)?;

    // Old numeric URLs (and any non-canonical path) redirect to the slug.
    if numeric_id.is_some()
        && let Some(ref slug) = team.slug
    {
        return Ok(Redirect::permanent(&format!("/team/{slug}")).into_response());
    }
    let team_id = team.id;

    let rows = sqlx::query!(
        r#"SELECT g.id,
                  g.date,
                  opp.title AS opponent_title,
                  opp.slug AS "opponent_slug?",
                  COALESCE(opp.slug, opp.id::STRING) AS "opponent_path!",
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
        team_id,
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
            opponent_title: row.opponent_title,
            opponent_slug: row.opponent_slug,
            opponent_path: row.opponent_path,
            our_score: row.our_score,
            their_score: row.their_score,
            result: result.to_string(),
            statsbook_drive_file_id: row.statsbook_drive_file_id,
        });
    }

    let league = derive_league(&team.title).to_string();
    let tmpl = state.env.get_template("team.html")?;
    let html = tmpl.render(minijinja::context! {
        team_id => team.id,
        title   => team.title,
        slug    => team.slug,
        region  => team.region,
        league,
        record,
        games,
    })?;
    Ok(Html(html).into_response())
}
