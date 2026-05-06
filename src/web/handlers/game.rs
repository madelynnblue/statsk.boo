use axum::extract::{Path, State};
use axum::response::Html;
use crate::models::GameData;
use crate::web::{AppState, error::AppError};

pub async fn handle(
    State(state): State<AppState>,
    Path(drive_file_id): Path<String>,
) -> Result<Html<String>, AppError> {
    let row = sqlx::query!(
        "SELECT date, data::text as data_text FROM games WHERE drive_file_id = $1",
        drive_file_id,
    )
    .fetch_optional(&*state.pool).await?
    .ok_or(AppError::NotFound)?;

    let game: GameData = serde_json::from_str(row.data_text.as_deref().unwrap_or_default())
        .map_err(anyhow::Error::from)?;

    let home_score = game.total_score("home");
    let away_score = game.total_score("away");

    let tmpl = state.env.get_template("game.html")?;
    let html = tmpl.render(minijinja::context! {
        date       => row.date.map(|d| d.to_string()).unwrap_or_default(),
        home_score,
        away_score,
        game       => game,
    })?;
    Ok(Html(html))
}
