use std::collections::HashMap;

use crate::models::{GameData, GameSummary, Penalty, SideStats, Skater, TeamData, Venue};
use crate::web::{AppState, error::AppError};
use axum::extract::{Path, State};
use axum::response::Html;
use serde::Serialize;

pub async fn handle(
    State(state): State<AppState>,
    Path(drive_file_id): Path<String>,
) -> Result<Html<String>, AppError> {
    let file_id = drive_file_id.clone();
    let row = sqlx::query!(
        r#"SELECT date, version, tournament, host_league,
                  venue_name, venue_city, venue_state,
                  periods, penalties
           FROM games WHERE drive_file_id = $1"#,
        drive_file_id,
    )
    .fetch_optional(&*state.pool)
    .await?
    .ok_or(AppError::NotFound)?;

    let side_rows = sqlx::query!(
        r#"SELECT side as "side!: String", league, team, color
           FROM game_sides WHERE drive_file_id = $1"#,
        drive_file_id,
    )
    .fetch_all(&*state.pool)
    .await?;
    if side_rows.len() != 2 {
        return Err(AppError::NotFound);
    }

    let skater_rows = sqlx::query!(
        r#"SELECT side as "side!: String", number, name
           FROM game_skaters WHERE drive_file_id = $1"#,
        drive_file_id,
    )
    .fetch_all(&*state.pool)
    .await?;

    let summary_rows = sqlx::query!(
        r#"SELECT side as "side!: String", stats
           FROM game_summary WHERE drive_file_id = $1"#,
        drive_file_id,
    )
    .fetch_all(&*state.pool)
    .await?;

    let home_side = side_rows
        .iter()
        .find(|s| s.side == "home")
        .expect("game must have home side");
    let away_side = side_rows
        .iter()
        .find(|s| s.side == "away")
        .expect("game must have away side");

    let home_skaters: Vec<Skater> = skater_rows
        .iter()
        .filter(|s| s.side == "home")
        .map(|s| Skater {
            number: s.number.clone(),
            name: s.name.clone(),
        })
        .collect();
    let away_skaters: Vec<Skater> = skater_rows
        .iter()
        .filter(|s| s.side == "away")
        .map(|s| Skater {
            number: s.number.clone(),
            name: s.name.clone(),
        })
        .collect();

    let game_summary = if let (Some(home_sum), Some(away_sum)) = (
        summary_rows.iter().find(|s| s.side == "home"),
        summary_rows.iter().find(|s| s.side == "away"),
    ) {
        let home_stats: SideStats =
            serde_json::from_value(home_sum.stats.clone()).map_err(anyhow::Error::from)?;
        let away_stats: SideStats =
            serde_json::from_value(away_sum.stats.clone()).map_err(anyhow::Error::from)?;
        Some(GameSummary {
            home_players: home_stats.players,
            away_players: away_stats.players,
            home_totals: home_stats.totals,
            away_totals: away_stats.totals,
        })
    } else {
        None
    };

    let game = GameData {
        version: row.version,
        venue: Venue {
            name: row.venue_name,
            city: row.venue_city,
            state: row.venue_state,
        },
        tournament: row.tournament,
        host_league: row.host_league,
        home: TeamData {
            league: home_side.league.clone(),
            team: home_side.team.clone(),
            color: home_side.color.clone(),
            skaters: home_skaters,
        },
        away: TeamData {
            league: away_side.league.clone(),
            team: away_side.team.clone(),
            color: away_side.color.clone(),
            skaters: away_skaters,
        },
        periods: serde_json::from_value(row.periods).map_err(anyhow::Error::from)?,
        penalties: serde_json::from_value(row.penalties).map_err(anyhow::Error::from)?,
        game_summary,
    };

    let home_score = game.total_score("home");
    let away_score = game.total_score("away");

    let home_names = skater_name_map(&game, "home");
    let away_names = skater_name_map(&game, "away");
    let home_penalties = group_penalties(&game.penalties, "home", &home_names);
    let away_penalties = group_penalties(&game.penalties, "away", &away_names);

    let tmpl = state.env.get_template("game.html")?;
    let html = tmpl.render(minijinja::context! {
        date       => row.date.map(|d| d.to_string()).unwrap_or_default(),
        home_score,
        away_score,
        game       => game,
        home_names,
        away_names,
        home_penalties,
        away_penalties,
        file_id,
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
    side: &str,
    names: &HashMap<String, String>,
) -> Vec<PlayerPenalties> {
    let mut groups: HashMap<String, Vec<&Penalty>> = HashMap::new();
    for p in penalties.iter().filter(|p| p.side == side) {
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

fn skater_name_map(game: &GameData, side: &str) -> HashMap<String, String> {
    let skaters = match side {
        "home" => &game.home.skaters,
        "away" => &game.away.skaters,
        _ => unreachable!(),
    };
    skaters
        .iter()
        .map(|s| (s.number.clone(), s.name.clone()))
        .collect()
}
