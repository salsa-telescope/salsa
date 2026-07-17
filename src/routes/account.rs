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
use crate::i18n::Language;
use crate::middleware::language::language_cookie;
use crate::middleware::session::clear_session_cookie;
use crate::models::user::User;
use crate::routes::index::render_main;

pub fn routes(state: AppState) -> Router {
    Router::new()
        .route("/", get(get_account))
        .route("/timezone", post(set_timezone))
        .route("/language", post(set_language))
        .route("/delete", post(delete_account))
        .with_state(state)
}

/// One option in the timezone picker.
struct TzOption {
    name: &'static str,
    selected: bool,
}

/// One option in the language picker.
struct LanguageOption {
    code: &'static str,
    native_name: &'static str,
    selected: bool,
}

#[derive(Template)]
#[template(path = "account.html")]
struct AccountTemplate {
    user: User,
    lang: Language,
    /// All IANA timezones for the picker (alphabetical), with the user's
    /// current one marked selected.
    timezones: Vec<TzOption>,
    /// Supported UI languages, with the user's effective one marked
    /// selected.
    languages: Vec<LanguageOption>,
    /// Set after a successful save so the page can confirm it.
    saved: bool,
}

impl AccountTemplate {
    fn new(user: User, lang: Language, saved: bool) -> Self {
        let current = user.tz();
        let timezones = chrono_tz::TZ_VARIANTS
            .iter()
            .map(|tz| TzOption {
                name: tz.name(),
                selected: *tz == current,
            })
            .collect();
        // Without a saved preference the picker shows the language the
        // page is being served in, which is what "selected" means to the
        // user looking at it.
        let effective = user.language.unwrap_or(lang);
        let languages = Language::ALL
            .iter()
            .map(|&language| LanguageOption {
                code: language.code(),
                native_name: language.native_name(),
                selected: language == effective,
            })
            .collect();
        AccountTemplate {
            user,
            lang,
            timezones,
            languages,
            saved,
        }
    }
}

async fn get_account(
    Extension(lang): Extension<Language>,
    Extension(user): Extension<Option<User>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, StatusCode> {
    let user = user.ok_or(StatusCode::UNAUTHORIZED)?;
    let content = AccountTemplate::new(user.clone(), lang, false)
        .render()
        .expect("Template rendering should always succeed");
    let content = if headers.get("hx-request").is_some() {
        content
    } else {
        render_main(Some(user), lang, content)
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
    Extension(lang): Extension<Language>,
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
    let content = AccountTemplate::new(user, lang, true)
        .render()
        .expect("Template rendering should always succeed");
    Ok(Html(content))
}

#[derive(Deserialize)]
struct LanguageForm {
    language: String,
}

/// Persist the user's preferred UI language from the profile-page picker.
/// Responds with HX-Refresh so htmx reloads the whole page — the header
/// and content must re-render in the new language, not just the card —
/// and refreshes the language cookie so the choice survives logout.
async fn set_language(
    Extension(user): Extension<Option<User>>,
    State(state): State<AppState>,
    Form(form): Form<LanguageForm>,
) -> Result<impl IntoResponse, StatusCode> {
    let user = user.ok_or(StatusCode::UNAUTHORIZED)?;
    let language = Language::from_code(&form.language).ok_or(StatusCode::BAD_REQUEST)?;
    User::set_language(state.database_connection.clone(), user.id, language.code())
        .await
        .map_err(|err| {
            error!("Failed to set language: {err:?}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    let mut response = Response::new(axum::body::Body::empty());
    response.headers_mut().insert(
        SET_COOKIE,
        HeaderValue::from_str(&language_cookie(language))
            .expect("Cookie built from fixed parts should always be a valid header"),
    );
    response
        .headers_mut()
        .insert("HX-Refresh", HeaderValue::from_static("true"));
    Ok(response)
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
    let clear_cookie = clear_session_cookie();
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
