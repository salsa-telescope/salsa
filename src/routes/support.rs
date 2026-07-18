use askama::Template;
use axum::{
    Extension, Router,
    extract::State,
    http::HeaderMap,
    response::{Html, IntoResponse},
    routing::get,
};

use crate::app::AppState;
use crate::i18n::Language;
use crate::models::support_announcement::fetch_support_announcement;
use crate::models::user::User;
use crate::routes::index::render_main;

pub fn routes(state: AppState) -> Router {
    Router::new()
        .route("/", get(get_support))
        .route("/manual", get(get_support_manual))
        .route("/google-sheets-guide", get(get_google_sheets_guide))
        .with_state(state)
}

async fn get_support_manual(
    Extension(lang): Extension<Language>,
    Extension(user): Extension<Option<User>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let content =
        crate::routes::read_content_page("user-manual", lang, "<p>User manual not available.</p>");
    let content = if headers.get("hx-request").is_some() {
        content
    } else {
        render_main(user, lang, content)
    };
    Html(content)
}

async fn get_google_sheets_guide(
    Extension(lang): Extension<Language>,
    Extension(user): Extension<Option<User>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let content = crate::routes::read_content_page(
        "google-sheets-guide",
        lang,
        "<p>Google Sheets guide not available.</p>",
    );
    let content = if headers.get("hx-request").is_some() {
        content
    } else {
        render_main(user, lang, content)
    };
    Html(content)
}

async fn get_support(
    Extension(lang): Extension<Language>,
    Extension(user): Extension<Option<User>>,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let body = crate::routes::read_content_page(
        "support",
        lang,
        "<p>Support information not available.</p>",
    );
    let announcement = fetch_support_announcement(state.database_connection)
        .await
        .ok()
        .flatten();
    let content = match announcement {
        Some(message) => format!("{}{}", render_announcement_banner(&message, lang), body),
        None => body,
    };
    let content = if headers.get("hx-request").is_some() {
        content
    } else {
        render_main(user, lang, content)
    };
    Html(content)
}

fn render_announcement_banner(message: &str, lang: Language) -> String {
    KnownIssueBanner { lang, message }
        .render()
        .expect("known_issue_banner")
}

#[derive(Template)]
#[template(path = "known_issue_banner.html")]
struct KnownIssueBanner<'a> {
    lang: Language,
    message: &'a str,
}
