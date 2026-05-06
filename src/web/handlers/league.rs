use crate::web::{AppState, error::AppError};
use axum::extract::{Query, State};
use axum::response::Html;
use serde::Deserialize;

#[derive(Deserialize)]
pub struct LeagueParams {
    pub league: String,
}

pub async fn handle(
    State(state): State<AppState>,
    Query(params): Query<LeagueParams>,
) -> Result<Html<String>, AppError> {
    let rows = sqlx::query!(
        r#"SELECT DISTINCT team FROM game_sides WHERE league = $1 ORDER BY 1"#,
        params.league,
    )
    .fetch_all(&*state.pool)
    .await?;

    let teams: Vec<String> = rows.iter().filter_map(|r| r.team.clone()).collect();

    let tmpl = state.env.get_template("league.html")?;
    let html = tmpl.render(minijinja::context! {
        league => params.league,
        teams,
    })?;
    Ok(Html(html))
}
