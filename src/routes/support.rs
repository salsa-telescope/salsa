use std::fs::read_to_string;

use axum::{
    Extension, Router,
    extract::State,
    http::HeaderMap,
    response::{Html, IntoResponse},
    routing::get,
};

use crate::app::AppState;
use crate::models::support_announcement::fetch_support_announcement;
use crate::models::user::User;
use crate::routes::index::render_main;

pub fn routes(state: AppState) -> Router {
    Router::new()
        .route("/", get(get_support))
        .route("/manual", get(get_support_manual))
        .with_state(state)
}

async fn get_support_manual(
    Extension(user): Extension<Option<User>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let content = read_to_string("assets/user-manual.html")
        .unwrap_or_else(|_| "<p>User manual not available.</p>".to_string());
    let content = if headers.get("hx-request").is_some() {
        content
    } else {
        render_main(user, content)
    };
    Html(content)
}

async fn get_support(
    Extension(user): Extension<Option<User>>,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let body = read_to_string("assets/support.html")
        .unwrap_or_else(|_| "<p>Support information not available.</p>".to_string());
    let announcement = fetch_support_announcement(state.database_connection)
        .await
        .ok()
        .flatten();
    let content = match announcement {
        Some(message) => format!("{}{}", render_announcement_banner(&message), body),
        None => body,
    };
    let content = if headers.get("hx-request").is_some() {
        content
    } else {
        render_main(user, content)
    };
    Html(content)
}

fn render_announcement_banner(message: &str) -> String {
    format!(
        r#"<div class="section light pb-0"><div class="border-l-4 border-warning-border bg-warning-light text-brand-dark rounded px-4 py-3"><div class="font-semibold mb-1">Known issue</div><p class="whitespace-pre-line">{message}</p></div></div>"#
    )
}
