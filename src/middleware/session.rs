use super::cookies::Cookies;
use crate::{app::AppState, models::session::Session};
use axum::{
    Extension,
    extract::{Request, State},
    http::{HeaderValue, StatusCode, header::SET_COOKIE},
    middleware::Next,
    response::Response,
};
use tracing::debug;

pub const SESSION_COOKIE_NAME: &str = "session";

// TODO: Stop leaking this to authentication.
pub fn get_session_token(Cookies(cookies_map): &Cookies) -> Option<String> {
    cookies_map.get(SESSION_COOKIE_NAME).cloned()
}

pub async fn session_middleware(
    State(state): State<AppState>,
    Extension(cookies): Extension<Cookies>,
    mut request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    debug!("Authenticating user session");
    let mut should_reset_cookie = false;
    let user = if let Some(session_token) = get_session_token(&cookies) {
        if let Some(session) =
            Session::fetch(state.database_connection.clone(), &session_token).await?
        {
            Some(session.user)
        } else {
            should_reset_cookie = true;
            None
        }
    } else {
        None
    };

    request.extensions_mut().insert(user);

    let mut response = next.run(request).await;

    // Assumes we only set cookies for the user session!
    if should_reset_cookie && response.headers().get(SET_COOKIE).is_none() {
        response.headers_mut().insert(
            SET_COOKIE,
            HeaderValue::from_str(&format!(
                "{SESSION_COOKIE_NAME}=deleted; expires=Thu, 01 Jan 1970 00:00:00 GMT"
            ))
            .expect("Hardcoded header value should always work"),
        );
    }

    Ok(response)
}
