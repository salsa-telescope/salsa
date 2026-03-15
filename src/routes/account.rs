use askama::Template;
use axum::{
    Extension, Router,
    extract::State,
    http::{HeaderMap, HeaderValue, StatusCode, header::SET_COOKIE},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};

use crate::app::AppState;
use crate::middleware::session::SESSION_COOKIE_NAME;
use crate::models::user::User;
use crate::routes::index::render_main;

pub fn routes(state: AppState) -> Router {
    Router::new()
        .route("/", get(get_account))
        .route("/delete", post(delete_account))
        .with_state(state)
}

#[derive(Template)]
#[template(path = "account.html", escape = "none")]
struct AccountTemplate {
    user: User,
}

async fn get_account(
    Extension(user): Extension<Option<User>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, StatusCode> {
    let user = user.ok_or(StatusCode::UNAUTHORIZED)?;
    let content = AccountTemplate { user: user.clone() }
        .render()
        .expect("Template rendering should always succeed");
    let content = if headers.get("hx-request").is_some() {
        content
    } else {
        render_main(Some(user), content)
    };
    Ok(Html(content))
}

async fn delete_account(
    Extension(user): Extension<Option<User>>,
    State(state): State<AppState>,
) -> Result<impl IntoResponse, StatusCode> {
    let user = user.ok_or(StatusCode::UNAUTHORIZED)?;
    log::info!(
        "Deleting account for user {} ({}, provider: {})",
        user.id,
        user.name,
        user.provider
    );
    user.delete(state.database_connection)
        .await
        .map_err(|err| {
            log::error!("Failed to delete account: {err:?}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    let clear_cookie =
        format!("{SESSION_COOKIE_NAME}=deleted; expires=Thu, 01 Jan 1970 00:00:00 GMT");
    let mut response = Response::new(axum::body::Body::empty());
    response.headers_mut().insert(
        SET_COOKIE,
        HeaderValue::from_str(&clear_cookie).expect("Hardcoded cookie value should always work"),
    );
    response
        .headers_mut()
        .insert("HX-Redirect", HeaderValue::from_static("/"));
    Ok(response)
}
