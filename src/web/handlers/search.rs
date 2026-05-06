use crate::web::{AppState, error::AppError};
use axum::extract::{Query, State};
use axum::response::Html;
use serde::Deserialize;

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
    date: chrono::NaiveDate,
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

    let (players, teams, leagues, games) = if q.len() >= 2 {
        let pattern = format!("%{}%", q);

        let player_rows = sqlx::query!(
            r#"SELECT DISTINCT gs.name, gs.number, gsi.league, gsi.team
               FROM game_skaters gs
               JOIN game_sides gsi ON gsi.drive_file_id = gs.drive_file_id AND gsi.side = gs.side
               WHERE gs.name ILIKE $1
               ORDER BY 4, 1
               LIMIT 200"#,
            &pattern,
        )
        .fetch_all(&*state.pool)
        .await?;

        let players: Vec<PlayerResult> = player_rows
            .iter()
            .map(|r| PlayerResult {
                league: r.league.clone().unwrap_or_default(),
                team: r.team.clone().unwrap_or_default(),
                name: r.name.clone(),
                number: r.number.clone(),
            })
            .collect();

        let team_rows = sqlx::query!(
            r#"SELECT DISTINCT league, team FROM game_sides
               WHERE league ILIKE $1 OR team ILIKE $1
               ORDER BY 1, 2
               LIMIT 200"#,
            &pattern,
        )
        .fetch_all(&*state.pool)
        .await?;

        let teams: Vec<TeamResult> = team_rows
            .iter()
            .map(|r| TeamResult {
                league: r.league.clone().unwrap_or_default(),
                team: r.team.clone().unwrap_or_default(),
            })
            .collect();

        let league_rows = sqlx::query!(
            r#"SELECT DISTINCT league FROM game_sides
               WHERE league ILIKE $1
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
            r#"SELECT DISTINCT ON (g.date, g.drive_file_id)
                      g.drive_file_id, g.date,
                      home.team as home_team, home.league as home_league,
                      away.team as away_team, away.league as away_league,
                      g.tournament, g.venue_name
               FROM games g
               JOIN game_sides home ON home.drive_file_id = g.drive_file_id AND home.side = 'home'
               JOIN game_sides away ON away.drive_file_id = g.drive_file_id AND away.side = 'away'
               WHERE g.tournament ILIKE $1
                  OR g.venue_name ILIKE $1
                  OR g.venue_city ILIKE $1
               ORDER BY g.date DESC, g.drive_file_id
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
