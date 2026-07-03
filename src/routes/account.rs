use askama::Template;
use axum::{
    Extension, Form, Router,
    extract::State,
    http::{HeaderMap, HeaderValue, StatusCode, header::SET_COOKIE},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;
use tracing::{error, info};

use crate::app::AppState;
use crate::middleware::session::SESSION_COOKIE_NAME;
use crate::models::user::User;
use crate::routes::index::render_main;

pub fn routes(state: AppState) -> Router {
    Router::new()
        .route("/", get(get_account))
        .route("/timezone", post(set_timezone))
        .route("/delete", post(delete_account))
        .with_state(state)
}

/// One option in the timezone picker.
struct TzOption {
    name: &'static str,
    selected: bool,
}

#[derive(Template)]
#[template(path = "account.html", escape = "none")]
struct AccountTemplate {
    user: User,
    /// All IANA timezones for the picker (alphabetical), with the user's
    /// current one marked selected.
    timezones: Vec<TzOption>,
    /// Set after a successful save so the page can confirm it.
    saved: bool,
}

impl AccountTemplate {
    fn new(user: User, saved: bool) -> Self {
        let current = user.tz();
        let timezones = chrono_tz::TZ_VARIANTS
            .iter()
            .map(|tz| TzOption {
                name: tz.name(),
                selected: *tz == current,
            })
            .collect();
        AccountTemplate {
            user,
            timezones,
            saved,
        }
    }
}

async fn get_account(
    Extension(user): Extension<Option<User>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, StatusCode> {
    let user = user.ok_or(StatusCode::UNAUTHORIZED)?;
    let content = AccountTemplate::new(user.clone(), false)
        .render()
        .expect("Template rendering should always succeed");
    let content = if headers.get("hx-request").is_some() {
        content
    } else {
        render_main(Some(user), content)
    };
    Ok(Html(content))
}

#[derive(Deserialize)]
struct TimezoneForm {
    timezone: String,
}

/// Persist the user's preferred timezone. Used both by the profile-page
/// picker (htmx) and by the one-shot browser-timezone auto-detect on first
/// login (plain `fetch`). Returns the refreshed Account card so htmx can
/// swap it in; the auto-detect caller ignores the body.
async fn set_timezone(
    Extension(user): Extension<Option<User>>,
    State(state): State<AppState>,
    Form(form): Form<TimezoneForm>,
) -> Result<impl IntoResponse, StatusCode> {
    let mut user = user.ok_or(StatusCode::UNAUTHORIZED)?;
    User::set_timezone(state.database_connection.clone(), user.id, &form.timezone)
        .await
        .map_err(|err| {
            error!("Failed to set timezone: {err:?}");
            StatusCode::BAD_REQUEST
        })?;
    // Reflect the new value in the re-rendered card.
    user.timezone = form.timezone.parse().ok();
    let content = AccountTemplate::new(user, true)
        .render()
        .expect("Template rendering should always succeed");
    Ok(Html(content))
}

async fn delete_account(
    Extension(user): Extension<Option<User>>,
    State(state): State<AppState>,
) -> Result<impl IntoResponse, StatusCode> {
    let user = user.ok_or(StatusCode::UNAUTHORIZED)?;
    info!(
        "Deleting account for user {} ({}, provider: {})",
        user.id, user.name, user.provider
    );
    user.delete(state.database_connection)
        .await
        .map_err(|err| {
            error!("Failed to delete account: {err:?}");
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
