use askama::Template;
use axum::{
    Extension, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use chrono::{NaiveDate, TimeZone, Utc};
use log::info;
use serde::Deserialize;

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
    telescopes: Vec<(String, bool, bool, bool)>, // (name, in_maintenance, is_booked_now, is_connected)
    usage_from: NaiveDate,
    usage_to: NaiveDate,
    total_bookings: usize,
    total_hours: i64,
    unique_users: usize,
}

async fn get_admin(
    Extension(user): Extension<Option<User>>,
    State(state): State<AppState>,
    Query(query): Query<UsageQuery>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, StatusCode> {
    let user = require_admin(user)?;
    let now = Utc::now();
    let usage_to = query.to.unwrap_or(now.date_naive());
    let usage_from = query
        .from
        .unwrap_or_else(|| (now - chrono::Duration::days(365)).date_naive());
    let from_dt = Utc.from_utc_datetime(&usage_from.and_hms_opt(0, 0, 0).unwrap());
    let to_dt = Utc.from_utc_datetime(&usage_to.succ_opt().unwrap().and_hms_opt(0, 0, 0).unwrap());

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
        let is_connected = if let Some(tel) = state.telescopes.get(&name).await {
            tel.get_info().await.is_ok_and(|i| {
                !matches!(
                    i.most_recent_error,
                    Some(
                        TelescopeError::TelescopeIOError(_) | TelescopeError::TelescopeNotConnected
                    )
                )
            })
        } else {
            false
        };
        telescopes.push((name, in_maintenance, is_booked_now, is_connected));
    }

    let bookings = Booking::fetch_in_range(state.database_connection, from_dt, to_dt)
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

    let content = AdminTemplate {
        telescopes,
        usage_from,
        usage_to,
        total_bookings,
        total_hours,
        unique_users,
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
struct UsageQuery {
    from: Option<NaiveDate>,
    to: Option<NaiveDate>,
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
