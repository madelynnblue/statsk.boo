use std::collections::HashMap;

use crate::models::{GameData, Penalty, Side, SideStats, Skater};
use crate::web::{AppState, error::AppError};
use axum::extract::{Path, State};
use axum::response::Html;
use serde::Serialize;

/// One WFTDA side of the game header. Identity and score come from
/// `wftda_games`/`wftda_teams`, never from the statsbook.
#[derive(Debug, Serialize)]
struct TeamHeader {
    id: i64,
    title: String,
    /// WFTDA slug, when the team has one.
    slug: Option<String>,
    /// Site path segment: the slug, or the numeric id as a fallback.
    path: String,
    score: i16,
}

/// Statsbook-derived detail, present only when `game_matches` links a
/// statsbook to this WFTDA game. Every field is keyed off the artifact's
/// `drive_file_id`, so the page renders without it (score-only mode).
#[derive(Debug, Serialize)]
struct GameDetail {
    drive_file_id: String,
    /// The WFTDA team each statsbook side resolved to (stage 6). `None` only
    /// if the side was left unassigned, which should not happen for a match.
    home_wftda_team_id: Option<i64>,
    away_wftda_team_id: Option<i64>,
    game: GameData,
    /// Rosters and summary are read from the normalized `statsbook_*` tables
    /// when present, falling back to the copy embedded in `game_data`.
    home_skaters: Vec<Skater>,
    away_skaters: Vec<Skater>,
    home_summary: Option<SideStats>,
    away_summary: Option<SideStats>,
    home_names: HashMap<String, String>,
    away_names: HashMap<String, String>,
    home_penalties: Vec<PlayerPenalties>,
    away_penalties: Vec<PlayerPenalties>,
}

pub async fn handle(
    State(state): State<AppState>,
    Path(wftda_game_id): Path<i64>,
) -> Result<Html<String>, AppError> {
    let row = sqlx::query!(
        r#"SELECT g.id, g.date,
                  g.team_1_id, g.team_2_id, g.team_1_score, g.team_2_score,
                  t1.title AS team_1_title,
                  t1.slug  AS "team_1_slug?",
                  t2.title AS team_2_title,
                  t2.slug  AS "team_2_slug?"
             FROM wftda_games g
             JOIN wftda_teams t1 ON t1.id = g.team_1_id
             JOIN wftda_teams t2 ON t2.id = g.team_2_id
            WHERE g.id = $1"#,
        wftda_game_id,
    )
    .fetch_optional(&*state.pool)
    .await?
    .ok_or(AppError::NotFound)?;

    let home = TeamHeader {
        id: row.team_1_id,
        title: row.team_1_title,
        path: row
            .team_1_slug
            .clone()
            .unwrap_or_else(|| row.team_1_id.to_string()),
        slug: row.team_1_slug,
        score: row.team_1_score,
    };
    let away = TeamHeader {
        id: row.team_2_id,
        title: row.team_2_title,
        path: row
            .team_2_slug
            .clone()
            .unwrap_or_else(|| row.team_2_id.to_string()),
        slug: row.team_2_slug,
        score: row.team_2_score,
    };

    let match_row = sqlx::query!(
        r#"SELECT m.drive_file_id,
                  sb.game_data AS "game_data?"
             FROM game_matches m
             JOIN statsbook_games sb ON sb.drive_file_id = m.drive_file_id
             JOIN statsbooks s ON s.drive_file_id = m.drive_file_id
            WHERE m.wftda_game_id = $1
            ORDER BY s.modified_time DESC NULLS LAST, m.drive_file_id
            LIMIT 1"#,
        wftda_game_id,
    )
    .fetch_optional(&*state.pool)
    .await?;

    let detail = match match_row {
        None => None,
        Some(m) => {
            let game: GameData = serde_json::from_value(m.game_data.ok_or_else(|| {
                anyhow::anyhow!("statsbook {} has no game_data", m.drive_file_id)
            })?)
            .map_err(anyhow::Error::from)?;

            let side_rows = sqlx::query!(
                r#"SELECT side AS "side!: String", wftda_team_id
                     FROM statsbook_sides
                    WHERE drive_file_id = $1"#,
                m.drive_file_id,
            )
            .fetch_all(&*state.pool)
            .await?;
            let mut home_wftda_team_id = None;
            let mut away_wftda_team_id = None;
            for side in side_rows {
                match side.side.as_str() {
                    "home" => home_wftda_team_id = side.wftda_team_id,
                    "away" => away_wftda_team_id = side.wftda_team_id,
                    _ => {}
                }
            }

            // Rosters: the normalized `statsbook_skaters` table is the query
            // surface; the embedded `GameData` roster is a fallback for rows
            // parsed before the table existed.
            let skater_rows = sqlx::query!(
                r#"SELECT side AS "side!: String", number, name
                     FROM statsbook_skaters
                    WHERE drive_file_id = $1
                    ORDER BY side, number"#,
                m.drive_file_id,
            )
            .fetch_all(&*state.pool)
            .await?;
            let (mut home_skaters, mut away_skaters) = (Vec::new(), Vec::new());
            for s in skater_rows {
                let skater = Skater {
                    number: s.number,
                    name: s.name,
                };
                match s.side.as_str() {
                    "home" => home_skaters.push(skater),
                    "away" => away_skaters.push(skater),
                    _ => {}
                }
            }
            if home_skaters.is_empty() {
                home_skaters = game.home.skaters.clone();
            }
            if away_skaters.is_empty() {
                away_skaters = game.away.skaters.clone();
            }

            let home_names: HashMap<String, String> = home_skaters
                .iter()
                .map(|s| (s.number.clone(), s.name.clone()))
                .collect();
            let away_names: HashMap<String, String> = away_skaters
                .iter()
                .map(|s| (s.number.clone(), s.name.clone()))
                .collect();
            let home_penalties = group_penalties(&game.penalties, Side::Home, &home_names);
            let away_penalties = group_penalties(&game.penalties, Side::Away, &away_names);

            // Game Summary likewise comes from `statsbook_summary` first.
            let summary_rows = sqlx::query!(
                r#"SELECT side AS "side!: String", stats
                     FROM statsbook_summary
                    WHERE drive_file_id = $1"#,
                m.drive_file_id,
            )
            .fetch_all(&*state.pool)
            .await?;
            let (mut home_summary, mut away_summary): (Option<SideStats>, Option<SideStats>) =
                (None, None);
            for s in summary_rows {
                let parsed: SideStats =
                    serde_json::from_value(s.stats).map_err(anyhow::Error::from)?;
                match s.side.as_str() {
                    "home" => home_summary = Some(parsed),
                    "away" => away_summary = Some(parsed),
                    _ => {}
                }
            }
            if home_summary.is_none() {
                home_summary = game.game_summary.as_ref().map(|g| SideStats {
                    players: g.home_players.clone(),
                    totals: g.home_totals.clone(),
                });
            }
            if away_summary.is_none() {
                away_summary = game.game_summary.as_ref().map(|g| SideStats {
                    players: g.away_players.clone(),
                    totals: g.away_totals.clone(),
                });
            }

            Some(GameDetail {
                drive_file_id: m.drive_file_id,
                home_wftda_team_id,
                away_wftda_team_id,
                game,
                home_skaters,
                away_skaters,
                home_summary,
                away_summary,
                home_names,
                away_names,
                home_penalties,
                away_penalties,
            })
        }
    };

    let tmpl = state.env.get_template("game.html")?;
    let html = tmpl.render(minijinja::context! {
        wftda_game_id,
        date  => row.date.to_string(),
        home,
        away,
        detail,
    })?;
    Ok(Html(html))
}

#[derive(Debug, Serialize)]
struct PlayerPenalties {
    number: String,
    name: String,
    total: usize,
    foul_out: bool,
    penalties: Vec<PenaltyItem>,
}

#[derive(Debug, Serialize)]
struct PenaltyItem {
    code: String,
    period: u8,
    jam: Option<u16>,
}

fn group_penalties(
    penalties: &[Penalty],
    side: Side,
    names: &HashMap<String, String>,
) -> Vec<PlayerPenalties> {
    let mut groups: HashMap<String, Vec<&Penalty>> = HashMap::new();
    for p in penalties.iter().filter(|p| p.side == side.as_str()) {
        groups.entry(p.number.clone()).or_default().push(p);
    }
    let mut result: Vec<PlayerPenalties> = groups
        .into_iter()
        .map(|(number, pens)| {
            let foul_out = pens.iter().any(|p| p.foul_out);
            let total = pens.len();
            let penalty_items: Vec<PenaltyItem> = pens
                .iter()
                .map(|p| PenaltyItem {
                    code: p.code.clone(),
                    period: p.period,
                    jam: p.jam,
                })
                .collect();
            let name = names
                .get(&number)
                .cloned()
                .unwrap_or_else(|| number.clone());
            PlayerPenalties {
                number,
                name,
                total,
                foul_out,
                penalties: penalty_items,
            }
        })
        .collect();
    result.sort_by(|a, b| a.number.cmp(&b.number));
    result
}
