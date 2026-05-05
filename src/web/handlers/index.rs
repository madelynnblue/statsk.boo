use axum::extract::State;
use axum::response::Html;
use crate::web::{AppState, error::AppError};

pub async fn handle(State(state): State<AppState>) -> Result<Html<String>, AppError> {
    let tmpl = state.env.get_template("index.html")?;
    let html = tmpl.render(minijinja::context! {})?;
    Ok(Html(html))
}
