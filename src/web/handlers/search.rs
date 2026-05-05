use axum::extract::{Query, State};
use axum::response::Html;
use serde::Deserialize;
use std::collections::HashSet;
use crate::models::GameData;
use crate::web::{AppState, error::AppError};

#[derive(Deserialize)]
pub struct SearchParams {
    pub q: Option<String>,
}

#[derive(serde::Serialize)]
struct PlayerResult {
    league: String,
    team: String,
    name: String,
    number: String,
}

#[derive(serde::Serialize)]
struct TeamResult {
    league: String,
    team: String,
}

pub async fn handle(
    State(state): State<AppState>,
    Query(params): Query<SearchParams>,
) -> Result<Html<String>, AppError> {
    let q = params.q.as_deref().unwrap_or("").trim().to_string();
    let q_lower = q.to_lowercase();

    let (players, teams) = if q.len() >= 2 {
        let pattern = format!("%{}%", q);

        let rows = sqlx::query_as::<_, GameRow>(
            "SELECT data FROM games WHERE player_search ILIKE $1 ORDER BY date DESC LIMIT 200",
        )
        .bind(&pattern)
        .fetch_all(&*state.pool).await?;

        let mut seen_players: HashSet<(String, String, String)> = HashSet::new();
        let mut players: Vec<PlayerResult> = Vec::new();

        for row in &rows {
            let game: GameData = serde_json::from_value(row.data.clone())
                .map_err(anyhow::Error::from)?;
            for side_data in [&game.home, &game.away] {
                let league = side_data.league.clone().unwrap_or_default();
                let team   = side_data.team.clone().unwrap_or_default();
                for skater in &side_data.skaters {
                    if skater.name.to_lowercase().contains(&q_lower) {
                        let key = (league.clone(), skater.name.clone(), skater.number.clone());
                        if seen_players.insert(key) {
                            players.push(PlayerResult {
                                league: league.clone(),
                                team: team.clone(),
                                name: skater.name.clone(),
                                number: skater.number.clone(),
                            });
                        }
                    }
                }
            }
        }

        let team_rows = sqlx::query_as::<_, GameRow>(
            "SELECT data FROM games WHERE team_search ILIKE $1 ORDER BY date DESC LIMIT 200",
        )
        .bind(&pattern)
        .fetch_all(&*state.pool).await?;

        let mut seen_teams: HashSet<(String, String)> = HashSet::new();
        let mut teams: Vec<TeamResult> = Vec::new();

        for row in &team_rows {
            let game: GameData = serde_json::from_value(row.data.clone())
                .map_err(anyhow::Error::from)?;
            for side_data in [&game.home, &game.away] {
                let league = side_data.league.clone().unwrap_or_default();
                let team   = side_data.team.clone().unwrap_or_default();
                let name_matches = league.to_lowercase().contains(&q_lower)
                    || team.to_lowercase().contains(&q_lower);
                if name_matches {
                    let key = (league.clone(), team.clone());
                    if seen_teams.insert(key) {
                        teams.push(TeamResult { league, team });
                    }
                }
            }
        }

        (players, teams)
    } else {
        (vec![], vec![])
    };

    let tmpl = state.env.get_template("search.html")?;
    let html = tmpl.render(minijinja::context! {
        query => q,
        players => players,
        teams => teams,
    })?;
    Ok(Html(html))
}

// Helper struct for sqlx row
#[derive(sqlx::FromRow)]
struct GameRow {
    data: serde_json::Value,
}
