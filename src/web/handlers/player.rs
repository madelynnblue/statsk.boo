use std::collections::{HashMap, HashSet};

use crate::canon::{best_name, canonicalize_name};
use crate::models::{GameData, SideStats, SummaryPlayer};
use crate::web::{AppState, error::AppError};
use axum::extract::{Query, State};
use axum::response::Html;
use serde::{Deserialize, Serialize};

/// Player identity is `(wftda_team_id, name_canonical, number)`: WFTDA has no
/// player entities, so the team id supplies the team grouping that the league
/// name used to.
#[derive(Deserialize)]
pub struct PlayerParams {
    pub team: i64,
    pub name: String,
    pub number: String,
}

#[derive(Serialize)]
struct GameRow {
    game_id: i64,
    date: String,
    opponent_team: String,
    opponent_team_path: String,
    side: String,
    jams_as_jammer: u16,
    jams_as_pivot: u16,
    jams_as_blocker: u16,
    score: String,
    outcome: String,
}

#[derive(Serialize)]
struct GameSummaryEntry {
    game_id: i64,
    date: String,
    opponent_team: String,
    opponent_team_path: String,
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
    let name_canonical = canonicalize_name(&params.name);

    let team = sqlx::query!(
        r#"SELECT id, title, slug AS "slug?",
                  COALESCE(slug, id::STRING) AS "path!"
             FROM wftda_teams
            WHERE id = $1"#,
        params.team,
    )
    .fetch_optional(&*state.pool)
    .await?
    .ok_or(AppError::NotFound)?;

    let rows = sqlx::query!(
        r#"SELECT sk.drive_file_id,
                  sk.side AS "side!: String",
                  sk.name,
                  g.home_score, g.away_score,
                  g.game_data AS "game_data?",
                  wg.id AS wftda_game_id,
                  wg.date,
                  opp.title AS opponent_title,
                  COALESCE(opp.slug, opp.id::STRING) AS "opponent_path!"
             FROM statsbook_skaters sk
             JOIN statsbook_sides s
               ON s.drive_file_id = sk.drive_file_id AND s.side = sk.side
             JOIN statsbook_games g ON g.drive_file_id = sk.drive_file_id
             JOIN game_matches m
               ON m.drive_file_id = sk.drive_file_id AND m.wftda_game_id IS NOT NULL
             JOIN wftda_games wg ON wg.id = m.wftda_game_id
             JOIN wftda_teams opp
               ON opp.id = CASE WHEN wg.team_1_id = $1 THEN wg.team_2_id ELSE wg.team_1_id END
            WHERE s.wftda_team_id = $1 AND sk.name_canonical = $2 AND sk.number = $3
            ORDER BY wg.date DESC, wg.id DESC"#,
        params.team,
        name_canonical,
        params.number,
    )
    .fetch_all(&*state.pool)
    .await?;

    let display_name =
        best_name(rows.iter().map(|r| r.name.as_str())).unwrap_or_else(|| params.name.clone());

    let mut career = CareerStats {
        games: 0,
        games_as_jammer: 0,
        games_as_pivot: 0,
        games_as_blocker: 0,
    };
    let mut game_rows: Vec<GameRow> = Vec::new();
    // Parallel to `game_rows`: the artifact key needed to find this game's
    // Game Summary row.
    let mut summary_keys: Vec<(String, String)> = Vec::new();
    let mut seen_games: HashSet<i64> = HashSet::new();

    for row in &rows {
        // A WFTDA game can have more than one statsbook attached; keep one row
        // per WFTDA game (rows are newest-first) so the history has no dupes.
        if !seen_games.insert(row.wftda_game_id) {
            continue;
        }

        let game: Option<GameData> = match &row.game_data {
            Some(value) => {
                Some(serde_json::from_value(value.clone()).map_err(anyhow::Error::from)?)
            }
            None => None,
        };

        let side = &row.side;
        let mut games_as_jammer = false;
        let mut games_as_pivot = false;
        let mut games_as_blocker = false;
        let mut jams_as_jammer = 0u16;
        let mut jams_as_pivot = 0u16;
        let mut jams_as_blocker = 0u16;
        if let Some(game) = &game {
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
        }

        let (our_score, their_score) = if side == "home" {
            (row.home_score, row.away_score)
        } else {
            (row.away_score, row.home_score)
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
            game_id: row.wftda_game_id,
            date: row.date.to_string(),
            opponent_team: row.opponent_title.clone(),
            opponent_team_path: row.opponent_path.clone(),
            side: side.clone(),
            jams_as_jammer,
            jams_as_pivot,
            jams_as_blocker,
            score,
            outcome: outcome.to_string(),
        });
        summary_keys.push((row.drive_file_id.clone(), side.clone()));
    }

    // Game Summary rows are keyed by the artifact's `drive_file_id`, not the
    // WFTDA game id.
    let mut summary_ids: Vec<String> = summary_keys.iter().map(|(id, _)| id.clone()).collect();
    summary_ids.sort();
    summary_ids.dedup();

    let mut game_summary_rows: Vec<GameSummaryEntry> = Vec::new();
    if !summary_ids.is_empty() {
        let summary_rows = sqlx::query!(
            r#"SELECT drive_file_id, side AS "side!: String", stats
                 FROM statsbook_summary
                WHERE drive_file_id = ANY($1)"#,
            &summary_ids,
        )
        .fetch_all(&*state.pool)
        .await?;

        let mut summary_map: HashMap<(String, String), Vec<SummaryPlayer>> = HashMap::new();
        for r in summary_rows {
            let side_stats: SideStats =
                serde_json::from_value(r.stats).map_err(anyhow::Error::from)?;
            summary_map.insert((r.drive_file_id, r.side), side_stats.players);
        }

        for (game_row, key) in game_rows.iter().zip(summary_keys.iter()) {
            if let Some(players) = summary_map.get(key) {
                // Match by number only: numbers are unique within a team, and
                // name normalization may differ between the IGRF roster and
                // summary sheet.
                if let Some(stats) = players.iter().find(|p| p.number == params.number) {
                    game_summary_rows.push(GameSummaryEntry {
                        game_id: game_row.game_id,
                        date: game_row.date.clone(),
                        opponent_team: game_row.opponent_team.clone(),
                        opponent_team_path: game_row.opponent_team_path.clone(),
                        stats: stats.clone(),
                    });
                }
            }
        }
    }

    let tmpl = state.env.get_template("player.html")?;
    let html = tmpl.render(minijinja::context! {
        team_id    => team.id,
        team_title => team.title,
        team_slug  => team.slug,
        team_path  => team.path,
        name       => display_name,
        number     => params.number,
        career,
        games      => game_rows,
        game_summary_rows,
    })?;
    Ok(Html(html))
}
