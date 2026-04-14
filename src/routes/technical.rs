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
    Router::new()
        .route("/", get(get_technical))
        .route("/rot2prog", get(get_rot2prog))
        .route("/lna", get(get_lna))
}

async fn get_rot2prog(
    Extension(user): Extension<Option<User>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let content = read_to_string("assets/rot2prog.html")
        .unwrap_or_else(|_| "<p>ROT2PROG documentation not available.</p>".to_string());
    let content = if headers.get("hx-request").is_some() {
        content
    } else {
        render_main(user, content)
    };
    Html(content)
}

async fn get_lna(
    Extension(user): Extension<Option<User>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let content = read_to_string("assets/lna.html")
        .unwrap_or_else(|_| "<p>LNA documentation not available.</p>".to_string());
    let content = if headers.get("hx-request").is_some() {
        content
    } else {
        render_main(user, content)
    };
    Html(content)
}

async fn get_technical(
    Extension(user): Extension<Option<User>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let content = read_to_string("assets/technical.html")
        .unwrap_or_else(|_| "<p>Technical information not available.</p>".to_string());
    let content = if headers.get("hx-request").is_some() {
        content
    } else {
        render_main(user, content)
    };
    Html(content)
}
