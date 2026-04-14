use std::net::SocketAddr;

use askama::Template;
use axum::{
    Extension, Router,
    extract::{ConnectInfo, Form, Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header::SET_COOKIE},
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
};
use oauth2::{
    AuthUrl, AuthorizationCode, ClientId, ClientSecret, CsrfToken, RedirectUrl, Scope,
    TokenResponse, TokenUrl, basic::BasicClient,
};
use rand::Rng;
use reqwest::header::USER_AGENT;
use rusqlite::Result;
use serde::Deserialize;
use serde_json::{Map, Value};
use std::time::Duration;
use tokio::time::sleep;
use tracing::{debug, info};

use crate::routes::index::render_main;
use crate::{app::AppState, error::InternalError};
use crate::{
    middleware::cookies::Cookies,
    models::session::{Session, complete_oauth2_login, start_oauth2_login},
};
use crate::{
    middleware::session::{SESSION_COOKIE_NAME, get_session_token},
    models::user::User,
};

pub fn routes(state: AppState) -> Router {
    Router::new()
        .route("/authorized", get(authenticate_from_oauth2))
        .route("/login", get(login))
        .route("/local", post(local_login))
        .route("/logout", get(logout))
        .route("/redirect/{provider_name}", get(redirect_to_auth_provider))
        .with_state(state)
}

async fn logout(
    State(state): State<AppState>,
    Extension(cookies): Extension<Cookies>,
) -> Result<impl IntoResponse, InternalError> {
    // TODO: We shouln't need to extract the session again here.
    if let Some(session_token) = get_session_token(&cookies) {
        let session = Session::fetch(state.database_connection.clone(), &session_token).await?;
        if let Some(session) = session {
            session.delete(state.database_connection.clone()).await?;
        }
    }
    // Session cookie will be cleared on redirect by session middleware.
    Ok(Redirect::to("/"))
}

// Basic Oath2 flow
//
// 1. User is prompted to select oauth2 provider.
// 2. User is redirected to OAuth2 provider.
// 3. User comes back with an authorization code.
// 4. We use that code to request an authorization token from oauth2 provider.
// 5. We use the token to get the identity of the user from oauth2 provider.

#[derive(Template)]
#[template(path = "login.html")]
struct SelectAuthProvider {
    provider_names: Vec<String>,
    error: bool,
    rate_limited: bool,
}

#[derive(Deserialize)]
struct LoginQuery {
    error: Option<String>,
}

// 1. User is prompted to select oauth2 provider.
async fn login(
    State(state): State<AppState>,
    Query(query): Query<LoginQuery>,
) -> Result<impl IntoResponse, InternalError> {
    let provider_names = state.secrets.get_auth_provider_names();
    let rate_limited = query.error.as_deref() == Some("rate_limited");
    let error = !rate_limited && query.error.is_some();
    let content = SelectAuthProvider {
        provider_names,
        error,
        rate_limited,
    }
    .render()
    .expect("Template rendering should always succeed");
    Ok(Html(render_main(None, content)))
}

#[derive(Deserialize)]
struct LocalLoginForm {
    username: String,
    password: String,
}

async fn local_login(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    State(state): State<AppState>,
    Form(form): Form<LocalLoginForm>,
) -> Result<Response, InternalError> {
    let ip = addr.ip();
    if state.login_rate_limiter.is_blocked(ip) {
        info!(ip = ip.to_string(), "rate limiting local login attempt");
        return Ok(Redirect::to("/auth/login?error=rate_limited").into_response());
    }

    let user = User::fetch_local_with_password(
        state.database_connection.clone(),
        &form.username,
        &form.password,
    )
    .await?;
    let Some(user) = user else {
        let random_delay: u64 = {
            let mut rng = rand::rng();
            rng.random_range(100..=1000)
        };
        // This make it harder using the timing of the response to try
        // to extract info such as if the user exists.
        sleep(Duration::from_millis(random_delay)).await;
        state.login_rate_limiter.record_failure(ip);
        info!(
            username = form.username.clone(),
            ip = ip.to_string(),
            "rejecting local login with incorrect username/password combination"
        );
        return Ok(Redirect::to("/auth/login?error=1").into_response());
    };

    state.login_rate_limiter.record_success(ip);
    info!(
        username = form.username.clone(),
        ip = ip.to_string(),
        "successful local login"
    );

    let session = Session::create(state.database_connection.clone(), &user).await?;
    let cookie = format!(
        "{SESSION_COOKIE_NAME}={}; SameSite=Lax; HttpOnly; Secure; Path=/; Max-Age=2592000",
        session.token
    );
    let mut headers = HeaderMap::new();
    headers.insert(
        SET_COOKIE,
        cookie.parse().expect("Cookie should be parseable always."),
    );
    Ok((headers, Redirect::to("/")).into_response())
}

// 2. We redirect the user to auth provider (e.g. Discord) where they authorize our app.
async fn redirect_to_auth_provider(
    Path(provider): Path<String>,
    State(state): State<AppState>,
) -> Result<impl IntoResponse, InternalError> {
    // To know that we're the originator of the request when the user comes back from OAuth2 provider

    let auth_provider = state.secrets.get_auth_provider(&provider)?;
    let client = BasicClient::new(ClientId::new(auth_provider.client_id.clone()))
        .set_auth_uri(
            AuthUrl::new(auth_provider.auth_uri.clone()).expect("Hardcoded URL should always work"),
        )
        .set_redirect_uri(
            RedirectUrl::new(auth_provider.redirect_uri.clone())
                .expect("Hardcoded URL should always work."),
        );
    let (url, token) = auth_provider
        .scopes
        .iter()
        .fold(client.authorize_url(CsrfToken::new_random), |req, scope| {
            req.add_scope(Scope::new(scope.clone()))
        })
        .url();

    start_oauth2_login(state.database_connection.clone(), &provider, &token).await?;
    let cookie = format!(
        "{}={}; SameSite=Lax; HttpOnly; Secure; Path=/",
        SESSION_COOKIE_NAME,
        token.secret()
    );

    let mut headers = HeaderMap::new();
    headers.insert(
        SET_COOKIE,
        HeaderValue::from_str(&cookie).expect("cookie will always be valid"),
    );

    info!("Sending user to {provider} to authenticate");

    Ok((headers, Redirect::to(url.as_ref())))
}

#[derive(Debug, Deserialize)]
struct AuthRequest {
    code: String,
    // We store the CSRF token in the state.
    state: String,
}

// 3. User comes back with an authorization code.
async fn authenticate_from_oauth2(
    Query(query): Query<AuthRequest>,
    State(state): State<AppState>,
) -> Result<Response, InternalError> {
    debug!("Coming back from OAuth2 provider");
    let provider_name =
        match complete_oauth2_login(state.database_connection.clone(), &query.state).await {
            Ok(provider) => provider,
            Err(err) => {
                debug!("Failed to validate CSRF token from oauth2 provider: {err:?}");
                return Ok(StatusCode::UNAUTHORIZED.into_response());
            }
        };

    // 4. We use that code to request an authorization token from oauth2 provider.
    let http_client = reqwest::ClientBuilder::new()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("Hardcoded client should always build.");

    let provider = state.secrets.get_auth_provider(&provider_name)?;
    let client = BasicClient::new(ClientId::new(provider.client_id.clone()))
        .set_client_secret(ClientSecret::new(provider.client_secret.clone()))
        .set_redirect_uri(
            RedirectUrl::new(provider.redirect_uri.clone())
                .expect("Hardcoded URL should always work."),
        )
        .set_token_uri(
            TokenUrl::new(provider.token_uri.clone()).expect("Hardcoded URL should always work."),
        );
    let token = match client
        .exchange_code(AuthorizationCode::new(query.code.clone()))
        .request_async(&http_client)
        .await
    {
        Ok(token) => token,
        Err(err) => {
            debug!("Failed to get token from {provider_name}: {err}");
            // This realistically happens because of an old or bogus request
            // to this endpoint. Returning 401 is reasonable.
            return Ok(StatusCode::UNAUTHORIZED.into_response());
        }
    };

    debug!("Code authenticated");

    // 5. We use the token to get the identity of the user from oauth2 provider.
    let user_data: Map<String, Value> = http_client
        .get(&provider.user_uri)
        .header(USER_AGENT, "salsa/1.0.0")
        .bearer_auth(token.access_token().secret())
        .send()
        .await
        .map_err(|err| {
            InternalError::new(format!("Failed to fetch token from {provider_name}: {err}"))
        })?
        .json::<Map<String, Value>>()
        .await
        .map_err(|err| {
            InternalError::new(format!(
                "Failed to deserialize user from {provider_name}: {err}"
            ))
        })?;

    let user_id = match user_data.get(&provider.id_field) {
        Some(Value::Number(user_id)) => format!("{}", user_id),
        Some(Value::String(user_id)) => user_id.clone(),
        None => {
            return Err(InternalError::new(format!(
                "No id field in user object returned from {}",
                &provider.user_uri
            )));
        }
        _ => {
            return Err(InternalError::new(format!(
                "Id field in user object returned from {} had unexpected type",
                &provider.user_uri
            )));
        }
    };

    let user = match User::fetch_with_user_with_external_id(
        state.database_connection.clone(),
        provider_name.clone(),
        &user_id,
    )
    .await?
    {
        Some(user) => user,
        None => {
            debug!("Create new user");
            let Some(Some(username)) = user_data
                .get(&provider.display_name_field)
                .map(|username| username.as_str())
            else {
                return Err(InternalError::new(format!(
                    "No {} field in user object returned from {}",
                    &provider.display_name_field, &provider.user_uri
                )));
            };
            User::create_from_external(
                state.database_connection.clone(),
                username.to_string(),
                provider_name.to_string(),
                &user_id,
            )
            .await?
        }
    };

    let session = Session::create(state.database_connection.clone(), &user).await?;
    let cookie = session.token;
    // Note: We reuse the same session cookie name here. So we don't need to
    // reset that cookie.
    let cookie = format!(
        "{SESSION_COOKIE_NAME}={cookie}; SameSite=Lax; HttpOnly; Secure; Path=/; Max-Age=2592000"
    );

    let mut headers = HeaderMap::new();
    headers.insert(
        SET_COOKIE,
        cookie.parse().expect("Cookie should be parseable always."),
    );
    Ok((headers, Redirect::to("/")).into_response())
}
