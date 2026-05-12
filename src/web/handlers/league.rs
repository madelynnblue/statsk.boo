use crate::canon::{best_name, canonicalize_league};
use crate::web::{AppState, error::AppError};
use axum::extract::{Query, State};
use axum::response::Html;
use serde::Deserialize;
use std::collections::HashMap;

#[derive(Deserialize)]
pub struct LeagueParams {
    pub league: String,
}

#[derive(serde::Serialize)]
struct TeamEntry {
    team: String,
    team_canonical: String,
}

pub async fn handle(
    State(state): State<AppState>,
    Query(params): Query<LeagueParams>,
) -> Result<Html<String>, AppError> {
    let league_canonical = canonicalize_league(&params.league);

    let league_row = sqlx::query!(
        r#"SELECT league FROM game_sides WHERE league_canonical = $1 LIMIT 1"#,
        league_canonical,
    )
    .fetch_optional(&*state.pool)
    .await?
    .ok_or_else(|| AppError::NotFound)?;

    let team_rows = sqlx::query!(
        r#"SELECT team, team_canonical FROM game_sides WHERE league_canonical = $1"#,
        league_canonical,
    )
    .fetch_all(&*state.pool)
    .await?;

    let mut groups: HashMap<String, Vec<String>> = HashMap::new();
    for r in &team_rows {
        groups
            .entry(r.team_canonical.clone())
            .or_default()
            .push(r.team.clone().unwrap_or_default());
    }
    let mut teams: Vec<TeamEntry> = groups
        .into_iter()
        .filter_map(|(team_canonical, variants)| {
            Some(TeamEntry {
                team: best_name(variants.iter().map(|s| s.as_str()))?,
                team_canonical,
            })
        })
        .collect();
    teams.sort_by(|a, b| a.team_canonical.cmp(&b.team_canonical));

    let tmpl = state.env.get_template("league.html")?;
    let html = tmpl.render(minijinja::context! {
        league_canonical => league_canonical,
        league => league_row.league,
        teams,
    })?;
    Ok(Html(html))
}
