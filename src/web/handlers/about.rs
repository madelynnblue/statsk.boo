use crate::web::{AppState, error::AppError};
use axum::extract::State;
use axum::response::Html;

pub async fn handle(State(state): State<AppState>) -> Result<Html<String>, AppError> {
    let tmpl = state.env.get_template("about.html")?;
    let html = tmpl.render(minijinja::context! {})?;
    Ok(Html(html))
}
