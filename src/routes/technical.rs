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
    Router::new()
        .route("/", get(get_technical))
        .route("/rot2prog", get(get_rot2prog))
        .route("/lna", get(get_lna))
}

async fn get_rot2prog(
    Extension(lang): Extension<Language>,
    Extension(user): Extension<Option<User>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let content = crate::routes::read_content_page(
        "rot2prog",
        lang,
        "<p>ROT2PROG documentation not available.</p>",
    );
    let content = if headers.get("hx-request").is_some() {
        content
    } else {
        render_main(user, lang, content)
    };
    Html(content)
}

async fn get_lna(
    Extension(lang): Extension<Language>,
    Extension(user): Extension<Option<User>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let content =
        crate::routes::read_content_page("lna", lang, "<p>LNA documentation not available.</p>");
    let content = if headers.get("hx-request").is_some() {
        content
    } else {
        render_main(user, lang, content)
    };
    Html(content)
}

async fn get_technical(
    Extension(lang): Extension<Language>,
    Extension(user): Extension<Option<User>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let content = crate::routes::read_content_page(
        "technical",
        lang,
        "<p>Technical information not available.</p>",
    );
    let content = if headers.get("hx-request").is_some() {
        content
    } else {
        render_main(user, lang, content)
    };
    Html(content)
}
