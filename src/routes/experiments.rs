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
    Router::new().route("/hi", get(get_experiments_hi))
}

async fn get_experiments_hi(
    Extension(user): Extension<Option<User>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let content = read_to_string("assets/experiments-hi.html")
        .unwrap_or_else(|_| "<p>HI experiment page not available.</p>".to_string());
    let content = if headers.get("hx-request").is_some() {
        content
    } else {
        render_main(user, content)
    };
    Html(content)
}
