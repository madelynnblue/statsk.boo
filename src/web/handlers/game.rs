use std::collections::HashMap;

use crate::models::{GameData, Penalty};
use crate::web::{AppState, error::AppError};
use axum::extract::{Path, State};
use axum::response::Html;
use serde::Serialize;

pub async fn handle(
    State(state): State<AppState>,
    Path(canonical_id): Path<String>,
) -> Result<Html<String>, AppError> {
    let row = sqlx::query!(
        r#"SELECT id, date, source, game_data FROM games WHERE canonical_id = $1"#,
        canonical_id,
    )
    .fetch_optional(&*state.pool)
    .await?
    .ok_or(AppError::NotFound)?;

    let game: GameData = serde_json::from_value(row.game_data.ok_or(AppError::NotFound)?)
        .map_err(anyhow::Error::from)?;

    let home_score = game.home_score;
    let away_score = game.away_score;
    let home_names = skater_name_map(&game, "home");
    let away_names = skater_name_map(&game, "away");
    let home_penalties = group_penalties(&game.penalties, "home", &home_names);
    let away_penalties = group_penalties(&game.penalties, "away", &away_names);

    let tmpl = state.env.get_template("game.html")?;
    let html = tmpl.render(minijinja::context! {
        date       => row.date.to_string(),
        home_score,
        away_score,
        game       => game,
        home_names,
        away_names,
        home_penalties,
        away_penalties,
        game_id => canonical_id,
        drive_file_id => row.id,
        source => row.source,
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
