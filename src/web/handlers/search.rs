use crate::models::GameData;
use crate::web::{AppState, error::AppError};
use axum::extract::{Query, State};
use axum::response::Html;
use serde::Deserialize;
use std::collections::HashSet;

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

#[derive(serde::Serialize)]
struct GameResult {
    drive_file_id: String,
    date: Option<chrono::NaiveDate>,
    home_team: String,
    home_league: String,
    away_team: String,
    away_league: String,
    tournament: Option<String>,
    venue_name: Option<String>,
}

pub async fn handle(
    State(state): State<AppState>,
    Query(params): Query<SearchParams>,
) -> Result<Html<String>, AppError> {
    let q = params.q.as_deref().unwrap_or("").trim().to_string();
    let q_lower = q.to_lowercase();

    let (players, teams, leagues, games) = if q.len() >= 2 {
        let pattern = format!("%{}%", q);

        let rows = sqlx::query!(
            "SELECT data FROM games WHERE player_search ILIKE $1 ORDER BY date DESC LIMIT 200",
            &pattern,
        )
        .fetch_all(&*state.pool)
        .await?;

        let mut seen_players: HashSet<(String, String, String)> = HashSet::new();
        let mut players: Vec<PlayerResult> = Vec::new();

        for row in &rows {
            let game: GameData =
                serde_json::from_value(row.data.clone()).map_err(anyhow::Error::from)?;
            for side_data in [&game.home, &game.away] {
                let league = side_data.league.clone().unwrap_or_default();
                let team = side_data.team.clone().unwrap_or_default();
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

        let team_rows = sqlx::query!(
            "SELECT data FROM games WHERE team_search ILIKE $1 ORDER BY date DESC LIMIT 200",
            &pattern,
        )
        .fetch_all(&*state.pool)
        .await?;

        let mut seen_teams: HashSet<(String, String)> = HashSet::new();
        let mut teams: Vec<TeamResult> = Vec::new();

        for row in &team_rows {
            let game: GameData =
                serde_json::from_value(row.data.clone()).map_err(anyhow::Error::from)?;
            for side_data in [&game.home, &game.away] {
                let league = side_data.league.clone().unwrap_or_default();
                let team = side_data.team.clone().unwrap_or_default();
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

        let league_rows = sqlx::query!(
            r#"SELECT DISTINCT data->'home'->>'league' as league FROM games
               WHERE league_search ILIKE $1 AND data->'home'->>'league' ILIKE $1
               UNION
               SELECT DISTINCT data->'away'->>'league' as league FROM games
               WHERE league_search ILIKE $1 AND data->'away'->>'league' ILIKE $1
               ORDER BY 1"#,
            &pattern,
        )
        .fetch_all(&*state.pool)
        .await?;

        let leagues: Vec<String> = league_rows
            .iter()
            .filter_map(|r| r.league.clone())
            .collect();

        let game_rows = sqlx::query!(
            r#"SELECT drive_file_id, date,
                      data->'home'->>'team' as home_team,
                      data->'home'->>'league' as home_league,
                      data->'away'->>'team' as away_team,
                      data->'away'->>'league' as away_league,
                      data->>'tournament' as tournament,
                      data->'venue'->>'name' as venue_name
               FROM games
               WHERE data->>'tournament' ILIKE $1
                  OR data->'venue'->>'name' ILIKE $1
                  OR data->'venue'->>'city' ILIKE $1
               ORDER BY date DESC
               LIMIT 200"#,
            &pattern,
        )
        .fetch_all(&*state.pool)
        .await?;

        let games: Vec<GameResult> = game_rows
            .into_iter()
            .map(|r| GameResult {
                drive_file_id: r.drive_file_id,
                date: r.date,
                home_team: r.home_team.unwrap_or_default(),
                home_league: r.home_league.unwrap_or_default(),
                away_team: r.away_team.unwrap_or_default(),
                away_league: r.away_league.unwrap_or_default(),
                tournament: r.tournament,
                venue_name: r.venue_name,
            })
            .collect();

        (players, teams, leagues, games)
    } else {
        (vec![], vec![], vec![], vec![])
    };

    let tmpl = state.env.get_template("search.html")?;
    let html = tmpl.render(minijinja::context! {
        query => q,
        players => players,
        teams => teams,
        leagues => leagues,
        games => games,
    })?;
    Ok(Html(html))
}
