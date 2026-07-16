use super::cookies::Cookies;
use crate::{
    app::AppState,
    models::session::{SESSION_LIFETIME_SECS, Session},
};
use axum::{
    Extension,
    extract::{Request, State},
    http::{HeaderValue, StatusCode, header::SET_COOKIE},
    middleware::Next,
    response::Response,
};
use tracing::trace;

/// The `__Host-` prefix makes browsers reject any cookie with this name that
/// is not Secure, Path=/ and host-only. Without it, another service on a
/// parent domain (anything under chalmers.se) can set a cookie with the same
/// name that shadows ours on every request, making login impossible for
/// affected users.
pub const SESSION_COOKIE_NAME: &str = "__Host-session";

/// The session cookie sent on login. All attributes required by the
/// `__Host-` prefix must be present or browsers silently drop the cookie.
pub fn session_cookie(token: &str) -> String {
    format!(
        "{SESSION_COOKIE_NAME}={token}; SameSite=Lax; HttpOnly; Secure; Path=/; Max-Age={SESSION_LIFETIME_SECS}"
    )
}

/// Clears the session cookie. Must carry the same Secure/Path attributes as
/// [`session_cookie`], both to satisfy the `__Host-` prefix rules and so the
/// deletion targets the same cookie regardless of which URL triggered it.
pub fn clear_session_cookie() -> String {
    format!("{SESSION_COOKIE_NAME}=deleted; SameSite=Lax; HttpOnly; Secure; Path=/; Max-Age=0")
}

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
    trace!("Authenticating user session");
    let mut should_reset_cookie = false;
    let user = if let Some(session_token) = get_session_token(&cookies) {
        if let Some(session) =
            Session::fetch(state.database_connection.clone(), &session_token).await?
        {
            let mut user = session.user;
            user.is_admin = state.admin_config.user_ids.contains(&user.id);
            Some(user)
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
            HeaderValue::from_str(&clear_session_cookie())
                .expect("Hardcoded header value should always work"),
        );
    }

    Ok(response)
}
