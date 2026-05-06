use std::collections::HashMap;

use crate::models::{GameData, Penalty};
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
        "SELECT date, data::text as data_text FROM games WHERE drive_file_id = $1",
        drive_file_id,
    )
    .fetch_optional(&*state.pool)
    .await?
    .ok_or(AppError::NotFound)?;

    let game: GameData = serde_json::from_str(row.data_text.as_deref().unwrap_or_default())
        .map_err(anyhow::Error::from)?;

    let home_score = game.total_score("home");
    let away_score = game.total_score("away");

    let home_names = skater_name_map(&game, "home");
    let away_names = skater_name_map(&game, "away");
    let player_penalties = group_penalties(&game.penalties, &home_names, &away_names);
    let home_penalties: Vec<&PlayerPenalties> = player_penalties
        .iter()
        .filter(|p| p.side == "home")
        .collect();
    let away_penalties: Vec<&PlayerPenalties> = player_penalties
        .iter()
        .filter(|p| p.side == "away")
        .collect();

    let tmpl = state.env.get_template("game.html")?;
    let html = tmpl.render(minijinja::context! {
        date       => row.date.map(|d| d.to_string()).unwrap_or_default(),
        home_score,
        away_score,
        game       => game,
        home_names,
        away_names,
        player_penalties => player_penalties,
        home_penalties,
        away_penalties,
        file_id,
    })?;
    Ok(Html(html))
}

#[derive(Debug, Serialize)]
struct PlayerPenalties {
    side: String,
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
    home_names: &HashMap<String, String>,
    away_names: &HashMap<String, String>,
) -> Vec<PlayerPenalties> {
    let mut groups: HashMap<(String, String), Vec<&Penalty>> = HashMap::new();
    for p in penalties {
        groups
            .entry((p.side.clone(), p.number.clone()))
            .or_default()
            .push(p);
    }
    let mut result: Vec<PlayerPenalties> = groups
        .into_iter()
        .map(|((side, number), pens)| {
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
            let name = match side.as_str() {
                "home" => home_names
                    .get(&number)
                    .cloned()
                    .unwrap_or_else(|| number.clone()),
                _ => away_names
                    .get(&number)
                    .cloned()
                    .unwrap_or_else(|| number.clone()),
            };
            PlayerPenalties {
                side,
                number,
                name,
                total,
                foul_out,
                penalties: penalty_items,
            }
        })
        .collect();
    result.sort_by(|a, b| a.side.cmp(&b.side).then(a.number.cmp(&b.number)));
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
