use std::collections::HashMap;

use crate::canon::canonicalize_league;
use crate::models::{GameData, SideStats, SummaryPlayer};
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
    game_id: String,
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
    game_id: String,
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
    let league_canonical = canonicalize_league(&params.league);

    let rows = sqlx::query!(
        r#"SELECT g.id, g.canonical_id, g.date, g.game_data,
                  gs.side as "side!: String",
                  player_side.league, player_side.team,
                  opp.league as opp_league, opp.team as opp_team
           FROM games g
           JOIN game_skaters gs ON gs.game_id = g.id
           JOIN game_sides player_side ON player_side.game_id = g.id AND player_side.side = gs.side
           JOIN game_sides opp ON opp.game_id = g.id AND opp.side != gs.side
           WHERE player_side.league_canonical = $1 AND gs.name = $2 AND gs.number = $3
           ORDER BY g.date DESC"#,
        league_canonical,
        params.name,
        params.number,
    )
    .fetch_all(&*state.pool)
    .await?;

    let display_league = rows
        .first()
        .and_then(|r| r.league.clone())
        .unwrap_or_else(|| params.league.clone());
    let display_team = rows
        .first()
        .and_then(|r| r.team.clone())
        .unwrap_or_default();

    let mut game_rows: Vec<GameRow> = Vec::new();
    let mut career = CareerStats {
        games: 0,
        games_as_jammer: 0,
        games_as_pivot: 0,
        games_as_blocker: 0,
    };

    for row in &rows {
        let game: GameData = serde_json::from_value(
            row.game_data
                .clone()
                .ok_or_else(|| anyhow::anyhow!("missing game_data"))?,
        )
        .map_err(anyhow::Error::from)?;
        let side = &row.side;
        let opponent_team = row.opp_team.clone().unwrap_or_default();
        let opponent_league = row.opp_league.clone().unwrap_or_default();

        let mut games_as_jammer = false;
        let mut games_as_pivot = false;
        let mut games_as_blocker = false;
        let mut jams_as_jammer = 0u16;
        let mut jams_as_pivot = 0u16;
        let mut jams_as_blocker = 0u16;
        for jam in game.periods.iter().flat_map(|p| &p.jams) {
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

        let (our_score, their_score) = if side == "home" {
            (game.home_score, game.away_score)
        } else {
            (game.away_score, game.home_score)
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
            game_id: row.canonical_id.clone(),
            date: row.date.to_string(),
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

    let game_ids: Vec<String> = rows.iter().map(|r| r.id.clone()).collect();
    let mut game_summary_rows: Vec<GameSummaryEntry> = Vec::new();

    if !game_ids.is_empty() {
        let summary_rows = sqlx::query!(
            r#"SELECT game_id, side as "side!: String", stats
               FROM game_summary WHERE game_id = ANY($1)"#,
            &game_ids,
        )
        .fetch_all(&*state.pool)
        .await?;

        let mut summary_map: HashMap<(String, String), Vec<SummaryPlayer>> = HashMap::new();
        for r in summary_rows {
            let side_stats: SideStats =
                serde_json::from_value(r.stats).map_err(anyhow::Error::from)?;
            summary_map.insert((r.game_id, r.side), side_stats.players);
        }

        for (row, game_row) in rows.iter().zip(game_rows.iter()) {
            let key = (row.id.clone(), row.side.clone());
            if let Some(players) = summary_map.get(&key) {
                // Match by number only: numbers are unique within a team, and name
                // normalization may differ between the IGRF roster and summary sheet.
                if let Some(stats) = players.iter().find(|p| p.number == params.number) {
                    game_summary_rows.push(GameSummaryEntry {
                        game_id: row.canonical_id.clone(),
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
        league_canonical => league_canonical,
        league => display_league,
        team   => display_team,
        name   => params.name,
        number => params.number,
        career => career,
        games  => game_rows,
        game_summary_rows => game_summary_rows,
    })?;
    Ok(Html(html))
}
