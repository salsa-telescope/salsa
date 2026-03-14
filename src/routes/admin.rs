use askama::Template;
use axum::{
    Extension, Router,
    extract::{Path, State},
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use log::info;

use crate::app::AppState;
use crate::models::maintenance::{fetch_maintenance_set, set_maintenance};
use crate::models::user::User;
use crate::routes::index::render_main;

pub fn routes(state: AppState) -> Router {
    Router::new()
        .route("/", get(get_admin))
        .route("/telescope/{name}/toggle", post(toggle_maintenance))
        .with_state(state)
}

fn require_admin(user: Option<User>) -> Result<User, StatusCode> {
    let user = user.ok_or(StatusCode::UNAUTHORIZED)?;
    if !user.is_admin {
        return Err(StatusCode::FORBIDDEN);
    }
    Ok(user)
}

#[derive(Template)]
#[template(path = "admin.html", escape = "none")]
struct AdminTemplate {
    telescopes: Vec<(String, bool)>, // (name, in_maintenance)
}

async fn get_admin(
    Extension(user): Extension<Option<User>>,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, StatusCode> {
    let user = require_admin(user)?;
    let telescope_names = state.telescopes.get_names().await;
    let maintenance = fetch_maintenance_set(state.database_connection)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let telescopes = telescope_names
        .into_iter()
        .map(|name| {
            let in_maintenance = maintenance.contains(&name);
            (name, in_maintenance)
        })
        .collect();
    let content = AdminTemplate { telescopes }
        .render()
        .expect("Template rendering should always succeed");
    let content = if headers.get("hx-request").is_some() {
        content
    } else {
        render_main(Some(user), content)
    };
    Ok(Html(content))
}

async fn toggle_maintenance(
    Extension(user): Extension<Option<User>>,
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Response, StatusCode> {
    let user = require_admin(user)?;
    if !state.telescopes.contains_key(&name).await {
        return Err(StatusCode::NOT_FOUND);
    }
    let maintenance = fetch_maintenance_set(state.database_connection.clone())
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let currently_in_maintenance = maintenance.contains(&name);
    let new_state = !currently_in_maintenance;
    info!(
        "Admin {} ({}) set telescope {} maintenance: {}",
        user.name, user.provider, name, new_state
    );
    set_maintenance(state.database_connection, &name, new_state)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let mut response = Response::new(axum::body::Body::empty());
    response.headers_mut().insert(
        "HX-Redirect",
        HeaderValue::from_static("/admin"),
    );
    Ok(response)
}
