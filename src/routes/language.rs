use axum::{
    Extension, Form, Router,
    extract::State,
    http::{
        HeaderMap, HeaderValue, StatusCode,
        header::{REFERER, SET_COOKIE},
    },
    response::{IntoResponse, Redirect, Response},
    routing::post,
};
use serde::Deserialize;
use tracing::error;

use crate::app::AppState;
use crate::i18n::Language;
use crate::middleware::language::language_cookie;
use crate::models::user::User;

pub fn routes(state: AppState) -> Router {
    Router::new()
        .route("/", post(set_language))
        .with_state(state)
}

#[derive(Deserialize)]
struct LanguageForm {
    language: String,
}

/// Switch UI language from the header picker. Sets the language cookie for
/// everyone (so the choice survives logout) and additionally persists it to
/// the profile for logged-in non-guest users, then redirects back to the
/// page the form was submitted from.
async fn set_language(
    State(state): State<AppState>,
    Extension(user): Extension<Option<User>>,
    headers: HeaderMap,
    Form(form): Form<LanguageForm>,
) -> Result<Response, StatusCode> {
    let language = Language::from_code(&form.language).ok_or(StatusCode::BAD_REQUEST)?;
    if let Some(user) = user.filter(|user| user.provider != "guest") {
        User::set_language(state.database_connection.clone(), user.id, language.code())
            .await
            .map_err(|err| {
                error!("Failed to persist language: {err:?}");
                StatusCode::INTERNAL_SERVER_ERROR
            })?;
    }
    let mut response = Redirect::to(&referer_path(&headers)).into_response();
    response.headers_mut().insert(
        SET_COOKIE,
        HeaderValue::from_str(&language_cookie(language))
            .expect("Cookie built from fixed parts should always be a valid header"),
    );
    Ok(response)
}

/// Where to send the user after switching: the page they came from, reduced
/// to its path + query so the redirect can never leave this origin.
fn referer_path(headers: &HeaderMap) -> String {
    let Some(referer) = headers.get(REFERER).and_then(|value| value.to_str().ok()) else {
        return "/".to_string();
    };
    // "//host/path" is protocol-relative, i.e. another origin — reject it.
    if referer.starts_with('/') && !referer.starts_with("//") {
        return referer.to_string();
    }
    referer
        .find("://")
        .map(|scheme_end| &referer[scheme_end + 3..])
        .and_then(|rest| rest.find('/').map(|path_start| &rest[path_start..]))
        .unwrap_or("/")
        .to_string()
}

#[cfg(test)]
mod test {
    use super::*;

    fn headers_with_referer(value: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(REFERER, HeaderValue::from_str(value).unwrap());
        headers
    }

    #[test]
    fn missing_referer_goes_home() {
        assert_eq!("/", referer_path(&HeaderMap::new()));
    }

    #[test]
    fn absolute_referer_is_reduced_to_path() {
        assert_eq!(
            "/bookings?week=2",
            referer_path(&headers_with_referer(
                "https://salsa.example/bookings?week=2"
            ))
        );
    }

    #[test]
    fn path_referer_is_kept() {
        assert_eq!("/observe", referer_path(&headers_with_referer("/observe")));
    }

    #[test]
    fn protocol_relative_referer_is_rejected() {
        assert_eq!("/", referer_path(&headers_with_referer("//evil.example/x")));
    }

    #[test]
    fn origin_only_referer_goes_home() {
        assert_eq!(
            "/",
            referer_path(&headers_with_referer("https://salsa.example"))
        );
    }
}
