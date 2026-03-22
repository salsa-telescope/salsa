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
