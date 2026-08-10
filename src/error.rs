use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use tracing::error;

#[derive(Debug)]
pub struct InternalError {
    pub message: String,
}

impl InternalError {
    pub fn new(message: String) -> InternalError {
        InternalError { message }
    }
}

/// Lets the mechanical steps of a query — prepare, query_map, reading a row —
/// use `?` instead of a `map_err` whose message only restates what the
/// rusqlite error already says. Call sites where the operation itself is worth
/// naming ("Failed to anonymize user") should keep building the message
/// explicitly: that context is what makes a journal line actionable, and it is
/// not recoverable from the sqlite error alone.
impl From<rusqlite::Error> for InternalError {
    fn from(err: rusqlite::Error) -> Self {
        InternalError::new(format!("Database error: {err}"))
    }
}

impl IntoResponse for InternalError {
    fn into_response(self) -> Response {
        StatusCode::from(self).into_response()
    }
}

impl From<InternalError> for StatusCode {
    fn from(value: InternalError) -> Self {
        // (thak): I find it somewhat dubious to log here in the conversion
        // function ... but I can't deny it's convenient.
        error!(
            "Error encountered while processiong request: {}",
            value.message
        );
        StatusCode::INTERNAL_SERVER_ERROR
    }
}
