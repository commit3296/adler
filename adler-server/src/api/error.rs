use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

/// JSON error envelope returned by failing handlers.
#[derive(Debug, Serialize)]
pub(super) struct ApiError {
    #[serde(skip)]
    status: StatusCode,
    error: &'static str,
    message: String,
}

impl ApiError {
    pub(super) fn bad_request(code: &'static str, msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            error: code,
            message: msg.into(),
        }
    }

    pub(super) fn not_found(code: &'static str, msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            error: code,
            message: msg.into(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = self.status;
        (status, Json(self)).into_response()
    }
}
