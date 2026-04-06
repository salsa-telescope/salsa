use askama::Template;
use axum::{
    Extension, Router,
    extract::{Form, Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
};
use chrono::{NaiveDate, TimeZone, Utc};
use serde::Deserialize;
use tracing::info;

use crate::app::AppState;
use crate::models::booking::Booking;
use crate::models::maintenance::{fetch_maintenance_set, set_maintenance};
use crate::models::telescope_types::TelescopeError;
use crate::models::user::User;
use crate::routes::index::render_main;

pub fn routes(state: AppState) -> Router {
    Router::new()
        .route("/", get(get_admin))
        .route("/telescope/{name}/toggle", post(toggle_maintenance))
        .route("/local-users", post(create_local_user_handler))
        .route("/local-users/{id}/delete", post(delete_local_user_handler))
        .route(
            "/local-users/{id}/password",
            post(set_local_password_handler),
        )
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
    telescopes: Vec<(String, bool, bool, bool, Option<bool>)>, // (name, in_maintenance, is_booked_now, controller_connected, receiver_connected)
    usage_from: NaiveDate,
    usage_to: NaiveDate,
    total_bookings: usize,
    total_hours: i64,
    unique_users: usize,
    local_users: Vec<(i64, String, String)>, // (id, username, comment)
    local_user_error: Option<String>,
}

async fn get_admin(
    Extension(user): Extension<Option<User>>,
    State(state): State<AppState>,
    Query(query): Query<AdminQuery>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, StatusCode> {
    let user = require_admin(user)?;
    let now = Utc::now();
    let local_user_error = query.error.clone();
    let usage_to = query.to.unwrap_or(now.date_naive());
    let usage_from = query
        .from
        .unwrap_or_else(|| (now - chrono::Duration::days(365)).date_naive());
    let from_dt = Utc.from_utc_datetime(&usage_from.and_hms_opt(0, 0, 0).unwrap());
    let to_dt = Utc.from_utc_datetime(
        &usage_to
            .succ_opt()
            .unwrap_or(usage_to)
            .and_hms_opt(0, 0, 0)
            .unwrap(),
    );

    let telescope_names = state.telescopes.get_names().await;
    let maintenance = fetch_maintenance_set(state.database_connection.clone())
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let active_bookings = Booking::fetch_active(state.database_connection.clone())
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let mut telescopes = Vec::new();
    for name in telescope_names {
        let in_maintenance = maintenance.contains(&name);
        let is_booked_now = active_bookings.iter().any(|b| b.telescope_name == name);
        let info = if let Some(tel) = state.telescopes.get(&name).await {
            tel.get_info().await.ok()
        } else {
            None
        };
        let is_connected = info.as_ref().is_some_and(|i| {
            !matches!(
                i.most_recent_error,
                Some(TelescopeError::TelescopeIOError(_) | TelescopeError::TelescopeNotConnected)
            )
        });
        let receiver_connected = info.and_then(|i| i.receiver_connected);
        telescopes.push((
            name,
            in_maintenance,
            is_booked_now,
            is_connected,
            receiver_connected,
        ));
    }

    let bookings = Booking::fetch_in_range(state.database_connection.clone(), from_dt, to_dt)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let total_bookings = bookings.len();
    let total_hours = bookings
        .iter()
        .map(|b| (b.end_time - b.start_time).num_hours())
        .sum();
    let unique_users = bookings
        .iter()
        .map(|b| b.user_id)
        .collect::<std::collections::HashSet<_>>()
        .len();
    let local_users = User::fetch_all_local(state.database_connection)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let content = AdminTemplate {
        telescopes,
        usage_from,
        usage_to,
        total_bookings,
        total_hours,
        unique_users,
        local_users,
        local_user_error,
    }
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
struct AdminQuery {
    from: Option<NaiveDate>,
    to: Option<NaiveDate>,
    error: Option<String>,
}

#[derive(Deserialize)]
struct CreateLocalUserForm {
    username: String,
    password: String,
    comment: String,
}

async fn create_local_user_handler(
    Extension(user): Extension<Option<User>>,
    State(state): State<AppState>,
    Form(form): Form<CreateLocalUserForm>,
) -> Result<Response, StatusCode> {
    require_admin(user)?;
    match User::create_local(
        state.database_connection,
        form.username.trim().to_string(),
        form.password,
        form.comment.trim().to_string(),
    )
    .await
    {
        Ok(_) => Ok(Redirect::to("/admin").into_response()),
        Err(err) if err.message.contains("already exists") => {
            Ok(Redirect::to("/admin?error=username_taken").into_response())
        }
        Err(_) => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

async fn delete_local_user_handler(
    Extension(user): Extension<Option<User>>,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Response, StatusCode> {
    require_admin(user)?;
    User::delete_local_by_id(state.database_connection, id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Redirect::to("/admin").into_response())
}

#[derive(Deserialize)]
struct SetPasswordForm {
    password: String,
}

async fn set_local_password_handler(
    Extension(user): Extension<Option<User>>,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Form(form): Form<SetPasswordForm>,
) -> Result<Response, StatusCode> {
    require_admin(user)?;
    User::set_local_password(state.database_connection, id, form.password)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Redirect::to("/admin").into_response())
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
    response
        .headers_mut()
        .insert("HX-Redirect", HeaderValue::from_static("/admin"));
    Ok(response)
}
