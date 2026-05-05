use axum::extract::{Query, State};
use axum::response::Html;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use crate::models::GameData;
use crate::web::{AppState, error::AppError};

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
    jams_as_jammer: u16,
    points_as_jammer: i16,
    penalties: usize,
}

#[derive(Serialize)]
struct CareerStats {
    games: usize,
    jams_as_jammer: u32,
    points_as_jammer: i32,
    total_penalties: usize,
}

pub async fn handle(
    State(state): State<AppState>,
    Query(params): Query<PlayerParams>,
) -> Result<Html<String>, AppError> {
    let rows = sqlx::query(
        r#"SELECT drive_file_id, date, data FROM games
           WHERE data @> jsonb_build_object(
               'home', jsonb_build_object(
                   'league', $1::text,
                   'skaters', jsonb_build_array(jsonb_build_object('number', $3::text, 'name', $2::text))
               )
           )
           OR data @> jsonb_build_object(
               'away', jsonb_build_object(
                   'league', $1::text,
                   'skaters', jsonb_build_array(jsonb_build_object('number', $3::text, 'name', $2::text))
               )
           )
           ORDER BY date DESC"#,
    )
    .bind(&params.league)
    .bind(&params.name)
    .bind(&params.number)
    .fetch_all(&*state.pool).await?;

    let mut game_rows: Vec<GameRow> = Vec::new();
    let mut career = CareerStats { games: 0, jams_as_jammer: 0, points_as_jammer: 0, total_penalties: 0 };

    for row in &rows {
        let drive_file_id: String = row.try_get("drive_file_id")?;
        let date: Option<chrono::NaiveDate> = row.try_get("date")?;
        let data: serde_json::Value = row.try_get("data")?;

        let game: GameData = serde_json::from_value(data)
            .map_err(anyhow::Error::from)?;

        let side = if game.home.league.as_deref() == Some(&params.league)
            && game.home.skaters.iter().any(|s| s.number == params.number && s.name == params.name)
        { "home" } else { "away" };

        let opponent = if side == "home" { &game.away } else { &game.home };
        let opponent_team   = opponent.team.clone().unwrap_or_default();
        let opponent_league = opponent.league.clone().unwrap_or_default();

        let jams_as_jammer = game.periods.iter()
            .flat_map(|p| &p.jams)
            .filter(|j| {
                let js = if side == "home" { &j.home } else { &j.away };
                js.jammer.as_deref() == Some(&params.number)
            })
            .count() as u16;

        let points_as_jammer: i16 = game.periods.iter()
            .flat_map(|p| &p.jams)
            .filter(|j| {
                let js = if side == "home" { &j.home } else { &j.away };
                js.jammer.as_deref() == Some(&params.number)
            })
            .map(|j| if side == "home" { j.home.score } else { j.away.score })
            .sum();

        let penalties = game.penalties.iter()
            .filter(|p| p.side == side && p.number == params.number)
            .count();

        career.games += 1;
        career.jams_as_jammer += jams_as_jammer as u32;
        career.points_as_jammer += points_as_jammer as i32;
        career.total_penalties += penalties;

        game_rows.push(GameRow {
            drive_file_id,
            date: date.map(|d| d.to_string()).unwrap_or_default(),
            opponent_team,
            opponent_league,
            side: side.to_string(),
            jams_as_jammer,
            points_as_jammer,
            penalties,
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
