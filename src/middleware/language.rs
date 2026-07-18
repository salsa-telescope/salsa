use axum::{extract::Request, http::header::ACCEPT_LANGUAGE, middleware::Next, response::Response};

use super::cookies::Cookies;
use crate::{i18n::Language, models::user::User};

/// Cookie holding the language choice of visitors without a profile
/// preference. Unlike the session cookie it is not security sensitive and
/// must also work for anonymous visitors on plain-HTTP dev servers, so no
/// `__Host-` prefix and no Secure attribute.
pub const LANGUAGE_COOKIE_NAME: &str = "lang";

/// About a year: effectively "until changed", without being immortal.
const LANGUAGE_COOKIE_MAX_AGE_SECS: i64 = 365 * 24 * 60 * 60;

pub fn language_cookie(language: Language) -> String {
    format!(
        "{LANGUAGE_COOKIE_NAME}={}; SameSite=Lax; Path=/; Max-Age={LANGUAGE_COOKIE_MAX_AGE_SECS}",
        language.code()
    )
}

/// Resolve the request's UI language — profile setting, then language
/// cookie, then Accept-Language, then English — and expose it as
/// `Extension<Language>`. Must run after the session middleware so the
/// user's saved preference is present in the request extensions.
pub async fn language_middleware(mut request: Request, next: Next) -> Response {
    let user_preference = request
        .extensions()
        .get::<Option<User>>()
        .and_then(|user| user.as_ref())
        .and_then(|user| user.language);
    let cookie_preference = || {
        request
            .extensions()
            .get::<Cookies>()?
            .get_all(LANGUAGE_COOKIE_NAME)
            .iter()
            .find_map(|value| Language::from_code(value))
    };
    let header_preference = || {
        request
            .headers()
            .get(ACCEPT_LANGUAGE)?
            .to_str()
            .ok()
            .and_then(Language::from_accept_language)
    };
    let language = user_preference
        .or_else(cookie_preference)
        .or_else(header_preference)
        .unwrap_or_default();
    request.extensions_mut().insert(language);
    next.run(request).await
}
