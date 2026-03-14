use askama::Template;
use axum::{
    Extension, Router,
    http::StatusCode,
    response::{Html, IntoResponse},
    routing::get,
};

use crate::models::user::User;
use crate::routes::index::render_main;

pub fn routes() -> Router {
    Router::new().route("/", get(get_account))
}

#[derive(Template)]
#[template(path = "account.html", escape = "none")]
struct AccountTemplate {
    user: User,
}

async fn get_account(
    Extension(user): Extension<Option<User>>,
) -> Result<impl IntoResponse, StatusCode> {
    let user = user.ok_or(StatusCode::UNAUTHORIZED)?;
    let content = AccountTemplate { user: user.clone() }
        .render()
        .expect("Template rendering should always succeed");
    Ok(Html(render_main(Some(user), content)))
}
