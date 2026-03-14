use askama::Template;
use axum::{
    Extension, Router,
    extract::State,
    http::{HeaderValue, StatusCode, header::SET_COOKIE},
    response::{Html, IntoResponse, Redirect},
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
) -> Result<impl IntoResponse, StatusCode> {
    let user = user.ok_or(StatusCode::UNAUTHORIZED)?;
    let content = AccountTemplate { user: user.clone() }
        .render()
        .expect("Template rendering should always succeed");
    Ok(Html(render_main(Some(user), content)))
}

async fn delete_account(
    Extension(user): Extension<Option<User>>,
    State(state): State<AppState>,
) -> Result<impl IntoResponse, StatusCode> {
    let user = user.ok_or(StatusCode::UNAUTHORIZED)?;
    user.delete(state.database_connection)
        .await
        .map_err(|err| {
            log::error!("Failed to delete account: {err:?}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    let clear_cookie =
        format!("{SESSION_COOKIE_NAME}=deleted; expires=Thu, 01 Jan 1970 00:00:00 GMT");
    let mut response = Redirect::to("/").into_response();
    response.headers_mut().insert(
        SET_COOKIE,
        HeaderValue::from_str(&clear_cookie).expect("Hardcoded cookie value should always work"),
    );
    Ok(response)
}
