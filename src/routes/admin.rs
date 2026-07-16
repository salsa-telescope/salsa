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
use crate::models::guest::GuestSession;
use crate::models::maintenance::{fetch_maintenance_set, set_maintenance};
use crate::models::support_announcement::{fetch_support_announcement, set_support_announcement};
use crate::models::telescope_types::TelescopeError;
use crate::models::user::User;
use crate::routes::index::render_main;

pub fn routes(state: AppState) -> Router {
    Router::new()
        .route("/", get(get_admin))
        .route("/telescope/{name}/toggle", post(toggle_maintenance))
        .route("/announcement", post(save_announcement_handler))
        .route("/local-users", post(create_local_user_handler))
        .route("/local-users/{id}/delete", post(delete_local_user_handler))
        .route(
            "/local-users/{id}/password",
            post(set_local_password_handler),
        )
        .route("/local-users/{id}/comment", post(set_local_comment_handler))
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
#[template(path = "admin.html")]
struct AdminTemplate {
    telescopes: Vec<(String, bool, bool, bool, Option<bool>)>, // (name, in_maintenance, is_booked_now, controller_connected, receiver_connected)
    usage_from: NaiveDate,
    usage_to: NaiveDate,
    total_bookings: usize,
    total_hours: i64,
    unique_users: usize,
    countries: Vec<(String, usize)>, // (country code, booking count), sorted by count desc
    guest_sessions_total: usize,
    guest_sessions_completed: usize,
    guest_session_total_minutes: i64,
    guest_session_median_seconds: Option<i64>,
    guest_end_reasons: Vec<(String, usize)>, // (reason, count) sorted by count desc
    guest_countries: Vec<(String, usize)>,   // (country code, count) sorted by count desc
    local_users: Vec<(i64, String, String)>, // (id, username, comment)
    local_user_error: Option<String>,
    announcement: String,
    users_by_provider: Vec<(String, usize)>, // (provider, count) sorted by count desc, guests excluded
    users_total: usize,
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
    let total_bookings = count_booking_segments(&bookings);
    let total_hours = bookings
        .iter()
        .map(|b| (b.end_time - b.start_time).num_hours())
        .sum();
    let unique_users = bookings
        .iter()
        .map(|b| b.user_id)
        .collect::<std::collections::HashSet<_>>()
        .len();
    let mut country_counts: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    for b in &bookings {
        if let Some(c) = &b.country {
            *country_counts.entry(c.clone()).or_default() += 1;
        }
    }
    let mut countries: Vec<(String, usize)> = country_counts.into_iter().collect();
    countries.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));

    // Guest session stats. Only count completed sessions for duration —
    // an in-flight session has an unknown duration. We still surface the
    // total count so admins can see "10 started this week, 8 completed".
    let guest_rows =
        GuestSession::fetch_in_range(state.database_connection.clone(), from_dt, to_dt)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let guest_sessions_total = guest_rows.len();
    let mut durations: Vec<i64> = guest_rows
        .iter()
        .filter_map(|g| g.ended_at.map(|e| (e - g.started_at).num_seconds()))
        .collect();
    let guest_sessions_completed = durations.len();
    let guest_session_total_minutes: i64 = durations.iter().sum::<i64>() / 60;
    durations.sort_unstable();
    let guest_session_median_seconds = if durations.is_empty() {
        None
    } else {
        Some(durations[durations.len() / 2])
    };
    let mut reason_counts: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    for g in &guest_rows {
        if let Some(r) = &g.end_reason {
            *reason_counts.entry(r.clone()).or_default() += 1;
        }
    }
    let mut guest_end_reasons: Vec<(String, usize)> = reason_counts.into_iter().collect();
    guest_end_reasons.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let mut guest_country_counts: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    for g in &guest_rows {
        if let Some(c) = &g.country {
            *guest_country_counts.entry(c.clone()).or_default() += 1;
        }
    }
    let mut guest_countries: Vec<(String, usize)> = guest_country_counts.into_iter().collect();
    guest_countries.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));

    let local_users = User::fetch_all_local(state.database_connection.clone())
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let users_by_provider = User::count_by_provider_non_guest(state.database_connection.clone())
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let users_total = users_by_provider.iter().map(|(_, c)| c).sum();
    let announcement = fetch_support_announcement(state.database_connection)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .unwrap_or_default();

    let content = AdminTemplate {
        telescopes,
        usage_from,
        usage_to,
        total_bookings,
        total_hours,
        unique_users,
        countries,
        guest_sessions_total,
        guest_sessions_completed,
        guest_session_total_minutes,
        guest_session_median_seconds,
        guest_end_reasons,
        guest_countries,
        local_users,
        local_user_error,
        announcement,
        users_by_provider,
        users_total,
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

#[derive(Deserialize)]
struct SetCommentForm {
    comment: String,
}

async fn set_local_comment_handler(
    Extension(user): Extension<Option<User>>,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Form(form): Form<SetCommentForm>,
) -> Result<Response, StatusCode> {
    require_admin(user)?;
    User::set_local_comment(
        state.database_connection,
        id,
        form.comment.trim().to_string(),
    )
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Redirect::to("/admin").into_response())
}

#[derive(Deserialize)]
struct AnnouncementForm {
    message: String,
}

async fn save_announcement_handler(
    Extension(user): Extension<Option<User>>,
    State(state): State<AppState>,
    Form(form): Form<AnnouncementForm>,
) -> Result<Response, StatusCode> {
    let admin = require_admin(user)?;
    let trimmed = form.message.trim();
    let stored = if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    };
    info!(
        "Admin {} ({}) updated support announcement (cleared: {})",
        admin.name,
        admin.provider,
        stored.is_none()
    );
    set_support_announcement(state.database_connection, stored)
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

/// Collapse runs of adjacent slots reserved by the same user on the same
/// telescope into a single booking. Since the calendar UI stores each slot
/// as its own row, the raw row count tracks total booked hours, not how
/// many distinct reservations users made.
fn count_booking_segments(bookings: &[Booking]) -> usize {
    if bookings.is_empty() {
        return 0;
    }
    let mut idx: Vec<usize> = (0..bookings.len()).collect();
    idx.sort_by(|&a, &b| {
        let ba = &bookings[a];
        let bb = &bookings[b];
        ba.telescope_name
            .cmp(&bb.telescope_name)
            .then(ba.user_id.cmp(&bb.user_id))
            .then(ba.start_time.cmp(&bb.start_time))
    });
    let mut segments = 1;
    for w in idx.windows(2) {
        let prev = &bookings[w[0]];
        let curr = &bookings[w[1]];
        if prev.telescope_name != curr.telescope_name
            || prev.user_id != curr.user_id
            || prev.end_time != curr.start_time
        {
            segments += 1;
        }
    }
    segments
}

#[cfg(test)]
mod test {
    use super::*;
    use chrono::DateTime;

    fn booking(user_id: i64, telescope: &str, start: i64, end: i64) -> Booking {
        Booking {
            id: 0,
            start_time: DateTime::from_timestamp(start, 0).unwrap(),
            end_time: DateTime::from_timestamp(end, 0).unwrap(),
            telescope_name: telescope.to_string(),
            user_id,
            user_name: String::new(),
            user_provider: String::new(),
            description: None,
            country: None,
        }
    }

    #[test]
    fn empty_input_is_zero_segments() {
        assert_eq!(count_booking_segments(&[]), 0);
    }

    #[test]
    fn single_booking_is_one_segment() {
        assert_eq!(count_booking_segments(&[booking(1, "vale", 0, 3600)]), 1);
    }

    #[test]
    fn adjacent_same_user_same_telescope_merges() {
        let rows = vec![
            booking(1, "vale", 0, 3600),
            booking(1, "vale", 3600, 7200),
            booking(1, "vale", 7200, 10800),
        ];
        assert_eq!(count_booking_segments(&rows), 1);
    }

    #[test]
    fn gap_breaks_segment() {
        let rows = vec![booking(1, "vale", 0, 3600), booking(1, "vale", 7200, 10800)];
        assert_eq!(count_booking_segments(&rows), 2);
    }

    #[test]
    fn different_user_breaks_segment() {
        let rows = vec![booking(1, "vale", 0, 3600), booking(2, "vale", 3600, 7200)];
        assert_eq!(count_booking_segments(&rows), 2);
    }

    #[test]
    fn different_telescope_breaks_segment() {
        let rows = vec![booking(1, "vale", 0, 3600), booking(1, "brage", 3600, 7200)];
        assert_eq!(count_booking_segments(&rows), 2);
    }

    #[test]
    fn input_order_does_not_matter() {
        let ordered = vec![booking(1, "vale", 0, 3600), booking(1, "vale", 3600, 7200)];
        let reversed = vec![booking(1, "vale", 3600, 7200), booking(1, "vale", 0, 3600)];
        assert_eq!(
            count_booking_segments(&ordered),
            count_booking_segments(&reversed)
        );
    }
}
