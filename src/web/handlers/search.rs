use std::collections::HashMap;

use crate::canon::{best_name, canonicalize_league};
use crate::web::handlers::leagues::derive_league;
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
    team_id: i64,
    team_title: String,
    team_slug: String,
    name: String,
    number: String,
}

#[derive(serde::Serialize)]
struct TeamResult {
    id: i64,
    title: String,
    slug: String,
    league: String,
}

#[derive(serde::Serialize)]
struct GameResult {
    wftda_game_id: i64,
    date: chrono::NaiveDate,
    home_slug: String,
    home_title: String,
    away_slug: String,
    away_title: String,
    home_score: i16,
    away_score: i16,
    tournament: Option<String>,
    venue_name: Option<String>,
    venue_city: Option<String>,
    venue_state: Option<String>,
}

pub async fn handle(
    State(state): State<AppState>,
    Query(params): Query<SearchParams>,
) -> Result<Html<String>, AppError> {
    let q = params.q.as_deref().unwrap_or("").trim().to_string();

    let (players, teams, leagues, games) = if q.len() >= 2 {
        let pattern = format!("%{}%", q);

        // --- WFTDA teams ---------------------------------------------------
        let team_rows = sqlx::query!(
            r#"SELECT id, title, COALESCE(slug, id::STRING) AS "slug!"
                 FROM wftda_teams
                WHERE title ILIKE $1
                ORDER BY title
                LIMIT 200"#,
            &pattern,
        )
        .fetch_all(&*state.pool)
        .await?;

        let teams: Vec<TeamResult> = team_rows
            .iter()
            .map(|r| TeamResult {
                id: r.id,
                title: r.title.clone(),
                slug: r.slug.clone(),
                league: derive_league(&r.title).to_string(),
            })
            .collect();

        // --- Leagues derived from matching team titles ---------------------
        let mut league_groups: HashMap<String, Vec<String>> = HashMap::new();
        for r in &team_rows {
            let league = derive_league(&r.title);
            if league.is_empty() {
                continue;
            }
            league_groups
                .entry(canonicalize_league(league))
                .or_default()
                .push(league.to_string());
        }
        let mut leagues: Vec<String> = league_groups
            .into_values()
            .filter_map(|variants| best_name(variants.iter().map(|s| s.as_str())))
            .collect();
        leagues.sort();

        // --- Statsbook players, scoped to matched (visible) games ----------
        let player_rows = sqlx::query!(
            r#"SELECT s.wftda_team_id AS "team_id!",
                      t.title AS team_title,
                      COALESCE(t.slug, t.id::STRING) AS "team_slug!",
                      sk.name, sk.name_canonical, sk.number
                 FROM statsbook_skaters sk
                 JOIN statsbook_sides s
                   ON s.drive_file_id = sk.drive_file_id AND s.side = sk.side
                 JOIN wftda_teams t ON t.id = s.wftda_team_id
                 JOIN game_matches m
                   ON m.drive_file_id = sk.drive_file_id AND m.wftda_game_id IS NOT NULL
                WHERE sk.name ILIKE $1
                ORDER BY t.title, sk.name, sk.number
                LIMIT 200"#,
            &pattern,
        )
        .fetch_all(&*state.pool)
        .await?;

        let mut player_groups: HashMap<(i64, String, String), (String, String, Vec<String>)> =
            HashMap::new();
        for r in &player_rows {
            let entry = player_groups
                .entry((r.team_id, r.name_canonical.clone(), r.number.clone()))
                .or_insert_with(|| (r.team_title.clone(), r.team_slug.clone(), Vec::new()));
            entry.2.push(r.name.clone());
        }
        let mut players: Vec<PlayerResult> = player_groups
            .into_iter()
            .map(
                |((team_id, _, number), (team_title, team_slug, names))| PlayerResult {
                    team_id,
                    team_title,
                    team_slug,
                    name: best_name(names.iter().map(|s| s.as_str())).unwrap_or_default(),
                    number,
                },
            )
            .collect();
        players.sort_by(|a, b| {
            a.name
                .cmp(&b.name)
                .then(a.number.cmp(&b.number))
                .then(a.team_title.cmp(&b.team_title))
        });

        // --- Games matched to a statsbook by tournament or venue -----------
        let game_rows = sqlx::query!(
            r#"SELECT * FROM (
                      SELECT DISTINCT ON (g.id)
                             g.id AS wftda_game_id,
                             g.date,
                             COALESCE(t1.slug, t1.id::STRING) AS "home_slug!",
                             t1.title AS home_title,
                             COALESCE(t2.slug, t2.id::STRING) AS "away_slug!",
                             t2.title AS away_title,
                             g.team_1_score AS home_score,
                             g.team_2_score AS away_score,
                             sg.tournament, sg.venue_name, sg.venue_city, sg.venue_state
                        FROM statsbook_games sg
                        JOIN game_matches m
                          ON m.drive_file_id = sg.drive_file_id
                         AND m.wftda_game_id IS NOT NULL
                        JOIN wftda_games g  ON g.id  = m.wftda_game_id
                        JOIN wftda_teams t1 ON t1.id = g.team_1_id
                        JOIN wftda_teams t2 ON t2.id = g.team_2_id
                       WHERE sg.tournament ILIKE $1
                          OR sg.venue_name ILIKE $1
                          OR sg.venue_city ILIKE $1
                       ORDER BY g.id, g.date DESC
               ) hits
               ORDER BY hits.date DESC, hits.wftda_game_id DESC
               LIMIT 200"#,
            &pattern,
        )
        .fetch_all(&*state.pool)
        .await?;

        let games: Vec<GameResult> = game_rows
            .into_iter()
            .map(|r| GameResult {
                wftda_game_id: r.wftda_game_id,
                date: r.date,
                home_slug: r.home_slug,
                home_title: r.home_title,
                away_slug: r.away_slug,
                away_title: r.away_title,
                home_score: r.home_score,
                away_score: r.away_score,
                tournament: r.tournament,
                venue_name: r.venue_name,
                venue_city: r.venue_city,
                venue_state: r.venue_state,
            })
            .collect();

        (players, teams, leagues, games)
    } else {
        (vec![], vec![], vec![], vec![])
    };

    let tmpl = state.env.get_template("search.html")?;
    let html = tmpl.render(minijinja::context! {
        query => q,
        players,
        teams,
        leagues,
        games,
    })?;
    Ok(Html(html))
}
