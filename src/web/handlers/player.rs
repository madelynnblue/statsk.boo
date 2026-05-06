use crate::models::GameData;
use crate::web::{AppState, error::AppError};
use axum::extract::{Query, State};
use axum::response::Html;
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
pub struct PlayerParams {
    pub league: String,
    pub name: String,
    pub number: String,
}

#[derive(Serialize)]
struct GameRow {
    drive_file_id: String,
    date: String,
    opponent_team: String,
    opponent_league: String,
    side: String,
    games_as_jammer: u16,
    games_as_pivot: u16,
    games_as_blocker: u16,
    jams_as_jammer: u16,
    jams_as_pivot: u16,
    jams_as_blocker: u16,
    score: String,
    outcome: String,
}

#[derive(Serialize)]
struct CareerStats {
    games: usize,
    games_as_jammer: usize,
    games_as_pivot: usize,
    games_as_blocker: usize,
}

pub async fn handle(
    State(state): State<AppState>,
    Query(params): Query<PlayerParams>,
) -> Result<Html<String>, AppError> {
    let rows = sqlx::query!(
        r#"SELECT drive_file_id as "drive_file_id!: String",
                  date,
                  data as "data!: serde_json::Value"
           FROM games
           WHERE data @> jsonb_build_object(
               'home', jsonb_build_object(
                   'league', $1::text,
                   'skaters', jsonb_build_array(jsonb_build_object('number', $3::text, 'name', $2::text))
               )
           )
           UNION ALL
           SELECT drive_file_id as "drive_file_id!: String",
                  date,
                  data as "data!: serde_json::Value"
           FROM games
           WHERE data @> jsonb_build_object(
               'away', jsonb_build_object(
                   'league', $1::text,
                   'skaters', jsonb_build_array(jsonb_build_object('number', $3::text, 'name', $2::text))
               )
           )
           ORDER BY date DESC"#,
        params.league,
        params.name,
        params.number,
    )
    .fetch_all(&*state.pool).await?;

    let mut game_rows: Vec<GameRow> = Vec::new();
    let mut career = CareerStats {
        games: 0,
        games_as_jammer: 0,
        games_as_pivot: 0,
        games_as_blocker: 0,
    };

    for row in &rows {
        let game: GameData =
            serde_json::from_value(row.data.clone()).map_err(anyhow::Error::from)?;

        let side = if game.home.league.as_deref() == Some(&params.league)
            && game
                .home
                .skaters
                .iter()
                .any(|s| s.number == params.number && s.name == params.name)
        {
            "home"
        } else {
            "away"
        };

        let opponent = if side == "home" {
            &game.away
        } else {
            &game.home
        };
        let opponent_team = opponent.team.clone().unwrap_or_default();
        let opponent_league = opponent.league.clone().unwrap_or_default();

        let mut games_as_jammer = false;
        let mut games_as_pivot = false;
        let mut games_as_blocker = false;
        let mut jams_as_jammer = 0u16;
        let mut jams_as_pivot = 0u16;
        let mut jams_as_blocker = 0u16;
        for jam in game.periods.iter().flat_map(|p| &p.jams) {
            let js = if side == "home" { &jam.home } else { &jam.away };
            let is_jammer = js.jammer.as_deref() == Some(&params.number);
            let is_pivot = js
                .lineup
                .iter()
                .any(|e| e.position == "pivot" && e.number == params.number);
            let is_blocker = js
                .lineup
                .iter()
                .any(|e| e.position == "blocker" && e.number == params.number);
            games_as_jammer |= is_jammer;
            games_as_pivot |= is_pivot;
            games_as_blocker |= is_blocker;
            if is_jammer {
                jams_as_jammer += 1;
            }
            if is_pivot {
                jams_as_pivot += 1;
            }
            if is_blocker {
                jams_as_blocker += 1;
            }
        }

        let home_score = game.total_score("home");
        let away_score = game.total_score("away");
        let (our_score, their_score) = if side == "home" {
            (home_score, away_score)
        } else {
            (away_score, home_score)
        };
        let score = format!("{}–{}", our_score, their_score);
        let outcome = if our_score > their_score {
            "W"
        } else if our_score < their_score {
            "L"
        } else {
            "T"
        };

        career.games += 1;
        if games_as_jammer {
            career.games_as_jammer += 1;
        }
        if games_as_pivot {
            career.games_as_pivot += 1;
        }
        if games_as_blocker {
            career.games_as_blocker += 1;
        }

        game_rows.push(GameRow {
            drive_file_id: row.drive_file_id.clone(),
            date: row.date.map(|d| d.to_string()).unwrap_or_default(),
            opponent_team,
            opponent_league,
            side: side.to_string(),
            games_as_jammer: games_as_jammer as u16,
            games_as_pivot: games_as_pivot as u16,
            games_as_blocker: games_as_blocker as u16,
            jams_as_jammer,
            jams_as_pivot,
            jams_as_blocker,
            score,
            outcome: outcome.to_string(),
        });
    }

    let tmpl = state.env.get_template("player.html")?;
    let html = tmpl.render(minijinja::context! {
        league => params.league,
        name   => params.name,
        number => params.number,
        career => career,
        games  => game_rows,
    })?;
    Ok(Html(html))
}
