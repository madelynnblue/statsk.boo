use std::collections::HashMap;

use crate::models::{Period, SideStats, SummaryPlayer, periods_score};
use crate::web::{AppState, error::AppError};
use axum::extract::{Query, State};
use axum::response::Html;
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
pub struct PlayerParams {
    pub league: String,
    pub name: String,
    pub number: String,
}

#[derive(Serialize)]
struct GameRow {
    drive_file_id: String,
    date: String,
    opponent_team: String,
    opponent_league: String,
    side: String,
    games_as_jammer: u16,
    games_as_pivot: u16,
    games_as_blocker: u16,
    jams_as_jammer: u16,
    jams_as_pivot: u16,
    jams_as_blocker: u16,
    score: String,
    outcome: String,
}

#[derive(Serialize)]
struct GameSummaryEntry {
    drive_file_id: String,
    date: String,
    opponent_team: String,
    opponent_league: String,
    stats: SummaryPlayer,
}

#[derive(Serialize)]
struct CareerStats {
    games: usize,
    games_as_jammer: usize,
    games_as_pivot: usize,
    games_as_blocker: usize,
}

pub async fn handle(
    State(state): State<AppState>,
    Query(params): Query<PlayerParams>,
) -> Result<Html<String>, AppError> {
    let rows = sqlx::query!(
        r#"SELECT g.drive_file_id, g.date, g.periods,
                  gs.side as "side!: String",
                  opp.league as opp_league, opp.team as opp_team
           FROM games g
           JOIN game_skaters gs ON gs.drive_file_id = g.drive_file_id
           JOIN game_sides player_side ON player_side.drive_file_id = g.drive_file_id AND player_side.side = gs.side
           JOIN game_sides opp ON opp.drive_file_id = g.drive_file_id AND opp.side != gs.side
           WHERE player_side.league = $1 AND gs.name = $2 AND gs.number = $3
           ORDER BY g.date DESC"#,
        params.league,
        params.name,
        params.number,
    )
    .fetch_all(&*state.pool).await?;

    let mut game_rows: Vec<GameRow> = Vec::new();
    let mut career = CareerStats {
        games: 0,
        games_as_jammer: 0,
        games_as_pivot: 0,
        games_as_blocker: 0,
    };

    for row in &rows {
        let periods: Vec<Period> =
            serde_json::from_value(row.periods.clone()).map_err(anyhow::Error::from)?;
        let side = &row.side;
        let opponent_team = row.opp_team.clone().unwrap_or_default();
        let opponent_league = row.opp_league.clone().unwrap_or_default();

        let mut games_as_jammer = false;
        let mut games_as_pivot = false;
        let mut games_as_blocker = false;
        let mut jams_as_jammer = 0u16;
        let mut jams_as_pivot = 0u16;
        let mut jams_as_blocker = 0u16;
        for jam in periods.iter().flat_map(|p| &p.jams) {
            let js = if side == "home" { &jam.home } else { &jam.away };
            let is_jammer = js.jammer.as_deref() == Some(&params.number);
            let is_pivot = js
                .lineup
                .iter()
                .any(|e| e.position == "pivot" && e.number == params.number);
            let is_blocker = js
                .lineup
                .iter()
                .any(|e| e.position == "blocker" && e.number == params.number);
            games_as_jammer |= is_jammer;
            games_as_pivot |= is_pivot;
            games_as_blocker |= is_blocker;
            if is_jammer {
                jams_as_jammer += 1;
            }
            if is_pivot {
                jams_as_pivot += 1;
            }
            if is_blocker {
                jams_as_blocker += 1;
            }
        }

        let home_score = periods_score(&periods, "home");
        let away_score = periods_score(&periods, "away");
        let (our_score, their_score) = if side == "home" {
            (home_score, away_score)
        } else {
            (away_score, home_score)
        };
        let score = format!("{}–{}", our_score, their_score);
        let outcome = if our_score > their_score {
            "W"
        } else if our_score < their_score {
            "L"
        } else {
            "T"
        };

        career.games += 1;
        if games_as_jammer {
            career.games_as_jammer += 1;
        }
        if games_as_pivot {
            career.games_as_pivot += 1;
        }
        if games_as_blocker {
            career.games_as_blocker += 1;
        }

        game_rows.push(GameRow {
            drive_file_id: row.drive_file_id.clone(),
            date: row.date.map(|d| d.to_string()).unwrap_or_default(),
            opponent_team,
            opponent_league,
            side: side.clone(),
            games_as_jammer: games_as_jammer as u16,
            games_as_pivot: games_as_pivot as u16,
            games_as_blocker: games_as_blocker as u16,
            jams_as_jammer,
            jams_as_pivot,
            jams_as_blocker,
            score,
            outcome: outcome.to_string(),
        });
    }

    let file_ids: Vec<String> = rows.iter().map(|r| r.drive_file_id.clone()).collect();
    let mut game_summary_rows: Vec<GameSummaryEntry> = Vec::new();

    if !file_ids.is_empty() {
        let summary_rows = sqlx::query!(
            r#"SELECT drive_file_id, side as "side!: String", stats
               FROM game_summary WHERE drive_file_id = ANY($1)"#,
            &file_ids,
        )
        .fetch_all(&*state.pool)
        .await?;

        let mut summary_map: HashMap<(String, String), Vec<SummaryPlayer>> = HashMap::new();
        for r in summary_rows {
            let side_stats: SideStats =
                serde_json::from_value(r.stats).map_err(anyhow::Error::from)?;
            summary_map.insert((r.drive_file_id, r.side), side_stats.players);
        }

        for (row, game_row) in rows.iter().zip(game_rows.iter()) {
            let key = (row.drive_file_id.clone(), row.side.clone());
            if let Some(players) = summary_map.get(&key) {
                // Match by number only: numbers are unique within a team, and name
                // normalization may differ between the IGRF roster and summary sheet.
                if let Some(stats) = players.iter().find(|p| p.number == params.number) {
                    game_summary_rows.push(GameSummaryEntry {
                        drive_file_id: row.drive_file_id.clone(),
                        date: game_row.date.clone(),
                        opponent_team: game_row.opponent_team.clone(),
                        opponent_league: game_row.opponent_league.clone(),
                        stats: stats.clone(),
                    });
                }
            }
        }
    }

    let tmpl = state.env.get_template("player.html")?;
    let html = tmpl.render(minijinja::context! {
        league => params.league,
        name   => params.name,
        number => params.number,
        career => career,
        games  => game_rows,
        game_summary_rows => game_summary_rows,
    })?;
    Ok(Html(html))
}
