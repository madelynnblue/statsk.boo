use axum::response::{IntoResponse, Response};
use axum::http::StatusCode;

#[derive(thiserror::Error, Debug)]
pub enum AppError {
    #[error("internal error: {0}")]
    Internal(#[from] anyhow::Error),

    #[error("not found")]
    NotFound,
}

impl From<minijinja::Error> for AppError {
    fn from(e: minijinja::Error) -> Self {
        AppError::Internal(e.into())
    }
}

impl From<sqlx::Error> for AppError {
    fn from(e: sqlx::Error) -> Self {
        AppError::Internal(e.into())
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, body) = match &self {
            AppError::Internal(e) => {
                tracing::error!("{e:#}");
                (StatusCode::INTERNAL_SERVER_ERROR, "internal error".to_string())
            }
            AppError::NotFound => (StatusCode::NOT_FOUND, "not found".to_string()),
        };
        (status, body).into_response()
    }
}
