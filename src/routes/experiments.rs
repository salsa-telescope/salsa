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
        .route("/hi", get(get_experiments_hi))
        .route("/gnss", get(get_experiments_gnss))
        .route("/sun", get(get_experiments_sun))
}

async fn get_experiments_sun(
    Extension(lang): Extension<Language>,
    Extension(user): Extension<Option<User>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let content = crate::routes::read_content_page(
        "experiments-sun",
        lang,
        "<p>Sun experiment page not available.</p>",
    );
    let content = if headers.get("hx-request").is_some() {
        content
    } else {
        render_main(user, lang, content)
    };
    Html(content)
}

async fn get_experiments_gnss(
    Extension(lang): Extension<Language>,
    Extension(user): Extension<Option<User>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let content = crate::routes::read_content_page(
        "experiments-gnss",
        lang,
        "<p>GNSS experiment page not available.</p>",
    );
    let content = if headers.get("hx-request").is_some() {
        content
    } else {
        render_main(user, lang, content)
    };
    Html(content)
}

async fn get_experiments_hi(
    Extension(lang): Extension<Language>,
    Extension(user): Extension<Option<User>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let content = crate::routes::read_content_page(
        "experiments-hi",
        lang,
        "<p>HI experiment page not available.</p>",
    );
    let content = if headers.get("hx-request").is_some() {
        content
    } else {
        render_main(user, lang, content)
    };
    Html(content)
}
