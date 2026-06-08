use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

use super::dto::DisabledSiteSummary;

/// JSON error envelope returned by failing handlers.
#[derive(Debug, Serialize)]
pub(super) struct ApiError {
    #[serde(skip)]
    status: StatusCode,
    error: &'static str,
    message: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    disabled_matches: Vec<DisabledSiteSummary>,
}

impl ApiError {
    pub(super) fn bad_request(code: &'static str, msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            error: code,
            message: msg.into(),
            disabled_matches: Vec::new(),
        }
    }

    pub(super) fn not_found(code: &'static str, msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            error: code,
            message: msg.into(),
            disabled_matches: Vec::new(),
        }
    }

    pub(super) fn with_disabled_matches(mut self, disabled: Vec<DisabledSiteSummary>) -> Self {
        self.disabled_matches = disabled;
        self
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = self.status;
        (status, Json(self)).into_response()
    }
}
