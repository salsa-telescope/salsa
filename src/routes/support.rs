use std::fs::read_to_string;

use axum::{
    Extension, Router,
    http::HeaderMap,
    response::{Html, IntoResponse},
    routing::get,
};

use crate::models::user::User;
use crate::routes::index::render_main;

pub fn routes() -> Router {
    Router::new().route("/", get(get_support))
}

async fn get_support(
    Extension(user): Extension<Option<User>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let content = read_to_string("assets/support.html")
        .unwrap_or_else(|_| "<p>Support information not available.</p>".to_string());
    let content = if headers.get("hx-request").is_some() {
        content
    } else {
        render_main(user, content)
    };
    Html(content)
}
