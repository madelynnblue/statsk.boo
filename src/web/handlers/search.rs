use std::collections::HashMap;

use crate::canon::{best_name, canonicalize_league, canonicalize_name, canonicalize_team};
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
    game_id: String,
    date: chrono::NaiveDate,
    home_team: String,
    home_league: String,
    away_team: String,
    away_league: String,
    home_score: i16,
    away_score: i16,
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
            r#"SELECT gs.name, gs.number, gsi.league
               FROM game_skaters gs
               INNER LOOKUP JOIN game_sides gsi ON gsi.game_id = gs.game_id AND gsi.side = gs.side
               WHERE gs.name ILIKE $1
               ORDER BY 3, 1
               LIMIT 200"#,
            &pattern,
        )
        .fetch_all(&*state.pool)
        .await?;

        let mut player_groups: HashMap<(String, String, String), Vec<(String, String)>> =
            HashMap::new();
        for r in &player_rows {
            let lc = canonicalize_league(r.league.as_deref().unwrap_or(""));
            let nc = canonicalize_name(&r.name);
            player_groups
                .entry((lc, nc, r.number.clone()))
                .or_default()
                .push((r.league.clone().unwrap_or_default(), r.name.clone()));
        }
        let mut players: Vec<PlayerResult> = player_groups
            .into_iter()
            .filter_map(|((_, _, number), variants)| {
                let league = best_name(variants.iter().map(|(l, _)| l.as_str()))?;
                let name = best_name(variants.iter().map(|(_, n)| n.as_str()))?;
                Some(PlayerResult {
                    league,
                    name,
                    number,
                })
            })
            .collect();
        players.sort_by(|a, b| a.league.cmp(&b.league).then(a.name.cmp(&b.name)));

        let team_rows = sqlx::query!(
            r#"SELECT league, team FROM game_sides
               WHERE league ILIKE $1 OR team ILIKE $1
               ORDER BY 1, 2
               LIMIT 200"#,
            &pattern,
        )
        .fetch_all(&*state.pool)
        .await?;

        let mut team_groups: HashMap<(String, String), Vec<(String, String)>> = HashMap::new();
        for r in &team_rows {
            let lc = canonicalize_league(r.league.as_deref().unwrap_or(""));
            let tc = canonicalize_team(r.league.as_deref(), r.team.as_deref().unwrap_or(""));
            team_groups.entry((lc, tc)).or_default().push((
                r.league.clone().unwrap_or_default(),
                r.team.clone().unwrap_or_default(),
            ));
        }
        let mut teams: Vec<TeamResult> = team_groups
            .into_values()
            .filter_map(|variants| {
                let league = best_name(variants.iter().map(|(l, _)| l.as_str()))?;
                let team = best_name(variants.iter().map(|(_, t)| t.as_str()))?;
                Some(TeamResult { league, team })
            })
            .collect();
        teams.sort_by(|a, b| a.league.cmp(&b.league).then(a.team.cmp(&b.team)));

        let league_rows = sqlx::query!(
            r#"SELECT DISTINCT league FROM game_sides
               WHERE league ILIKE $1
               ORDER BY 1"#,
            &pattern,
        )
        .fetch_all(&*state.pool)
        .await?;

        let mut league_groups: HashMap<String, Vec<String>> = HashMap::new();
        for r in &league_rows {
            if let Some(ref league) = r.league {
                let lc = canonicalize_league(league);
                league_groups.entry(lc).or_default().push(league.clone());
            }
        }
        let mut leagues: Vec<String> = league_groups
            .into_values()
            .filter_map(|variants| best_name(variants.iter().map(|s| s.as_str())))
            .collect();
        leagues.sort();

        let game_rows = sqlx::query!(
            r#"SELECT DISTINCT ON (g.date, g.id)
                      g.canonical_id, g.date, g.home_score, g.away_score,
                      home.team as home_team, home.league as home_league,
                      away.team as away_team, away.league as away_league,
                      g.tournament, g.venue_name
               FROM games g
               JOIN game_sides home ON home.game_id = g.id AND home.side = 'home'
               JOIN game_sides away ON away.game_id = g.id AND away.side = 'away'
               WHERE g.tournament ILIKE $1
                  OR g.venue_name ILIKE $1
                  OR g.venue_city ILIKE $1
               ORDER BY g.date DESC, g.id
               LIMIT 200"#,
            &pattern,
        )
        .fetch_all(&*state.pool)
        .await?;

        let games: Vec<GameResult> = game_rows
            .into_iter()
            .map(|r| GameResult {
                game_id: r.canonical_id,
                date: r.date,
                home_team: r.home_team.unwrap_or_default(),
                home_league: r.home_league.unwrap_or_default(),
                away_team: r.away_team.unwrap_or_default(),
                away_league: r.away_league.unwrap_or_default(),
                home_score: r.home_score,
                away_score: r.away_score,
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
