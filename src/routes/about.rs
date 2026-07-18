use axum::{
    Extension, Router,
    http::HeaderMap,
    response::{Html, IntoResponse},
    routing::get,
};

use crate::i18n::Language;
use crate::models::user::User;
use crate::routes::index::render_main;

pub fn routes() -> Router {
    Router::new().route("/", get(get_about))
}

async fn get_about(
    Extension(lang): Extension<Language>,
    Extension(user): Extension<Option<User>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let content =
        crate::routes::read_content_page("about", lang, "<p>About information not available.</p>");
    let content = if headers.get("hx-request").is_some() {
        content
    } else {
        render_main(user, lang, content)
    };
    Html(content)
}
