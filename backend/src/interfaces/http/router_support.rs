use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Serialize;
use thiserror::Error;
use tracing::{error, warn};

#[derive(Debug, Serialize)]
pub struct ApiErrorBody {
    pub error: String,
}

#[derive(Debug, Error)]
pub enum ApiError {
    #[error("bad request: {0}")]
    BadRequest(String),
    #[error("unauthorized")]
    Unauthorized,
    #[error("not found: {0}")]
    NotFound(String),
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("internal server error")]
    Internal,
}

impl ApiError {
    fn status_code(&self) -> StatusCode {
        match self {
            Self::BadRequest(_) => StatusCode::BAD_REQUEST,
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::NotFound(_) => StatusCode::NOT_FOUND,
            Self::Conflict(_) => StatusCode::CONFLICT,
            Self::Internal => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = self.status_code();
        let message = self.to_string();

        if status.is_server_error() {
            error!(%status, error = %message, "request failed");
        } else {
            warn!(%status, error = %message, "request rejected");
        }

        let body = Json(ApiErrorBody { error: message });

        (status, body).into_response()
    }
}
