use crate::canon::canonicalize_league;
use crate::web::handlers::leagues::group_leagues;
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
    let league_canonical = canonicalize_league(params.league.trim());

    let rows = sqlx::query!(
        r#"SELECT id, title, COALESCE(slug, id::STRING) AS "slug!" FROM wftda_teams ORDER BY title"#
    )
    .fetch_all(&*state.pool)
    .await?;

    let league = group_leagues(rows.into_iter().map(|r| (r.id, r.title, r.slug)).collect())
        .into_iter()
        .find(|l| l.league_canonical == league_canonical)
        .ok_or(AppError::NotFound)?;

    let tmpl = state.env.get_template("league.html")?;
    let html = tmpl.render(minijinja::context! {
        league => league.league,
        teams  => league.teams,
    })?;
    Ok(Html(html))
}
