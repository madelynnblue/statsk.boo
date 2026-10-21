use std::collections::HashMap;

use crate::canon::{best_name, canonicalize_league};
use crate::web::handlers::leagues::derive_league;
use crate::web::{AppState, error::AppError};
use axum::extract::{Query, State};
use axum::response::Html;
use serde::Deserialize;

/// Upper bound on matching skater rows fetched before grouping. One player can
/// contribute a row per game, so this is not the displayed limit; the rendered
/// player list is capped at [`PLAYER_RESULT_LIMIT`].
const PLAYER_HIT_CAP: i64 = 2000;
/// Rendered player rows (the previous single-statement `LIMIT`).
const PLAYER_RESULT_LIMIT: usize = 200;

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
        // Deliberately two statements. `name ILIKE '%…%'` makes CockroachDB
        // assume ~42% of the table matches no matter what the pattern is, so a
        // single statement joining the trigram hit straight to the visibility
        // tables plans as hash joins over full scans of `statsbook_sides` and
        // `game_matches`. The trigram index finds the few matching skaters on
        // its own; joining that result back by id (`= ANY`) has an exact
        // estimate and plans as primary-key lookups.
        let skater_hits = sqlx::query!(
            r#"SELECT drive_file_id,
                      side AS "side!: String",
                      name,
                      name_canonical,
                      number
                 FROM statsbook_skaters
                WHERE name ILIKE $1
                LIMIT $2"#,
            &pattern,
            PLAYER_HIT_CAP,
        )
        .fetch_all(&*state.pool)
        .await?;

        // Team attribution and the visibility rule, for the candidate files
        // only. A side is present here only when its game is matched.
        let mut hit_files: Vec<String> = skater_hits
            .iter()
            .map(|r| r.drive_file_id.clone())
            .collect();
        hit_files.sort();
        hit_files.dedup();

        let mut side_teams: HashMap<(String, String), (i64, String, String)> = HashMap::new();
        if !hit_files.is_empty() {
            let side_rows = sqlx::query!(
                r#"SELECT s.drive_file_id,
                          s.side AS "side!: String",
                          s.wftda_team_id AS "team_id!",
                          t.title AS team_title,
                          COALESCE(t.slug, t.id::STRING) AS "team_slug!"
                     FROM statsbook_sides s
                     JOIN wftda_teams t ON t.id = s.wftda_team_id
                     JOIN game_matches m
                       ON m.drive_file_id = s.drive_file_id
                      AND m.wftda_game_id IS NOT NULL
                    WHERE s.drive_file_id = ANY($1)"#,
                &hit_files,
            )
            .fetch_all(&*state.pool)
            .await?;
            for r in side_rows {
                side_teams.insert(
                    (r.drive_file_id, r.side),
                    (r.team_id, r.team_title, r.team_slug),
                );
            }
        }

        let mut player_groups: HashMap<(i64, String, String), (String, String, Vec<String>)> =
            HashMap::new();
        for r in &skater_hits {
            // No entry means the statsbook is unmatched (hidden) or the side
            // was never assigned to a team: it contributes nothing.
            let Some((team_id, team_title, team_slug)) =
                side_teams.get(&(r.drive_file_id.clone(), r.side.clone()))
            else {
                continue;
            };
            let entry = player_groups
                .entry((*team_id, r.name_canonical.clone(), r.number.clone()))
                .or_insert_with(|| (team_title.clone(), team_slug.clone(), Vec::new()));
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
        players.truncate(PLAYER_RESULT_LIMIT);

        // --- Games matched to a statsbook by tournament or venue -----------
        // Kept as one statement: a two-step split (candidate ids, then join)
        // measured slightly faster for trigram-served patterns (~6 ms vs ~8 ms)
        // but far slower for shorter ones — a `%xx%` pattern yields no trigram,
        // so the candidate query falls back to a full scan of `statsbook_games`
        // and drags the JSONB payload (270 ms, 317 MiB, vs ~28 ms here), because
        // this shape's LIMIT lets the lookup join stop early.
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
