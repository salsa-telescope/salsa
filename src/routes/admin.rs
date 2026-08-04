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
use crate::i18n::Language;
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
        .route(
            "/telescope/{name}/calibrate/preview",
            post(calibrate_preview_handler),
        )
        .route("/telescope/{name}/calibrate", post(calibrate_handler))
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

/// Caps on admin-entered free text. Generous for real use; they exist so
/// no request can stuff megabytes into the database or into argon2.
const MAX_USERNAME_CHARS: usize = 64;
const MAX_PASSWORD_BYTES: usize = 512;
const MAX_COMMENT_CHARS: usize = 500;
const MAX_ANNOUNCEMENT_CHARS: usize = 2000;

/// Typo guard for pointing calibration: real pointing offsets are a few
/// degrees at most, so anything larger is more likely a slipped decimal
/// point than a measurement.
const MAX_CALIBRATION_OFFSET_DEG: f64 = 10.0;

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
    /// Whether the booking figures below leave out admin-made bookings, so
    /// the checkbox can be re-rendered in the state that produced them.
    exclude_admins: bool,
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
    Extension(lang): Extension<Language>,
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
    // Admin bookings are often long maintenance and test slots, which swamp
    // the science usage they sit alongside. Dropped before anything is
    // counted, so every booking figure below reflects the same set.
    let exclude_admins = query.exclude_admins.unwrap_or(false);
    let bookings = if exclude_admins {
        without_admin_bookings(bookings, &state.admin_config.user_ids)
    } else {
        bookings
    };
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
        exclude_admins,
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
        render_main(Some(user), lang, content)
    };
    Ok(Html(content))
}

#[derive(Deserialize)]
struct AdminQuery {
    from: Option<NaiveDate>,
    to: Option<NaiveDate>,
    error: Option<String>,
    /// Set by the usage-report checkbox. Phrased as "exclude" rather than
    /// "include" so that its absence — an unticked box, or a bare /admin with
    /// no query string at all — means the report covers everyone, which is
    /// the total the page is expected to show by default.
    exclude_admins: Option<bool>,
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
    let username = form.username.trim().to_string();
    if username.chars().count() > MAX_USERNAME_CHARS
        || form.password.len() > MAX_PASSWORD_BYTES
        || form.comment.trim().chars().count() > MAX_COMMENT_CHARS
    {
        return Ok(Redirect::to("/admin?error=input_too_long").into_response());
    }
    match User::create_local(
        state.database_connection,
        username,
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
    if form.password.len() > MAX_PASSWORD_BYTES {
        return Ok(Redirect::to("/admin?error=input_too_long").into_response());
    }
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
    if form.comment.trim().chars().count() > MAX_COMMENT_CHARS {
        return Ok(Redirect::to("/admin?error=input_too_long").into_response());
    }
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
    let trimmed: String = form
        .message
        .trim()
        .chars()
        .take(MAX_ANNOUNCEMENT_CHARS)
        .collect();
    let stored = if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.as_str())
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

#[derive(Template)]
#[template(path = "admin_calibrate_confirm.html")]
struct CalibrateConfirmTemplate {
    name: String,
    az_offset_deg: f64,
    el_offset_deg: f64,
    current_az: String,
    current_el: String,
    new_az: String,
    new_el: String,
}

#[derive(Template)]
#[template(path = "admin_calibrate_result.html")]
struct CalibrateResultTemplate {
    name: String,
    error: Option<String>,
    previous_az: String,
    previous_el: String,
    adjusted_az: String,
    adjusted_el: String,
}

#[derive(Deserialize)]
struct CalibrateForm {
    az_offset_deg: f64,
    el_offset_deg: f64,
}

fn calibrate_error_response(name: &str, error: String) -> Response {
    Html(
        CalibrateResultTemplate {
            name: name.to_string(),
            error: Some(error),
            previous_az: String::new(),
            previous_el: String::new(),
            adjusted_az: String::new(),
            adjusted_el: String::new(),
        }
        .render()
        .expect("Template rendering should always succeed"),
    )
    .into_response()
}

/// Common guards for both calibration steps. Returns an error message to
/// show in the panel when the adjustment must not proceed.
async fn check_calibration_allowed(
    state: &AppState,
    name: &str,
    form: &CalibrateForm,
) -> Result<(), String> {
    if !form.az_offset_deg.is_finite() || !form.el_offset_deg.is_finite() {
        return Err("Offsets must be numbers.".to_string());
    }
    if form.az_offset_deg.abs() > MAX_CALIBRATION_OFFSET_DEG
        || form.el_offset_deg.abs() > MAX_CALIBRATION_OFFSET_DEG
    {
        return Err(format!(
            "Offsets larger than ±{MAX_CALIBRATION_OFFSET_DEG}° are refused as a typo guard."
        ));
    }
    if form.az_offset_deg == 0.0 && form.el_offset_deg == 0.0 {
        return Err("Both offsets are zero — nothing to adjust.".to_string());
    }
    let maintenance = fetch_maintenance_set(state.database_connection.clone())
        .await
        .map_err(|_| "Failed to read maintenance state.".to_string())?;
    if !maintenance.contains(name) {
        return Err("Telescope must be in maintenance mode before adjusting pointing.".to_string());
    }
    let active_bookings = Booking::fetch_active(state.database_connection.clone())
        .await
        .map_err(|_| "Failed to read bookings.".to_string())?;
    if active_bookings.iter().any(|b| b.telescope_name == name) {
        return Err("Telescope has an active booking — wait until the slot ends.".to_string());
    }
    Ok(())
}

fn format_deg(angle_rad: f64) -> String {
    format!("{:.1}", angle_rad.to_degrees())
}

async fn calibrate_preview_handler(
    Extension(user): Extension<Option<User>>,
    State(state): State<AppState>,
    Path(name): Path<String>,
    Form(form): Form<CalibrateForm>,
) -> Result<Response, StatusCode> {
    require_admin(user)?;
    let telescope = state
        .telescopes
        .get(&name)
        .await
        .ok_or(StatusCode::NOT_FOUND)?;
    if let Err(message) = check_calibration_allowed(&state, &name, &form).await {
        return Ok(calibrate_error_response(&name, message));
    }
    let current = match telescope.get_info().await {
        Ok(info) => info.current_horizontal,
        Err(err) => return Ok(calibrate_error_response(&name, err.to_string())),
    };
    let Some(current) = current else {
        return Ok(calibrate_error_response(
            &name,
            "The controller has not reported a position yet.".to_string(),
        ));
    };
    let content = CalibrateConfirmTemplate {
        current_az: format_deg(current.azimuth),
        current_el: format_deg(current.elevation),
        new_az: format_deg(current.azimuth - form.az_offset_deg.to_radians()),
        new_el: format_deg(current.elevation - form.el_offset_deg.to_radians()),
        name,
        az_offset_deg: form.az_offset_deg,
        el_offset_deg: form.el_offset_deg,
    }
    .render()
    .expect("Template rendering should always succeed");
    Ok(Html(content).into_response())
}

async fn calibrate_handler(
    Extension(user): Extension<Option<User>>,
    State(state): State<AppState>,
    Path(name): Path<String>,
    Form(form): Form<CalibrateForm>,
) -> Result<Response, StatusCode> {
    let admin = require_admin(user)?;
    let telescope = state
        .telescopes
        .get(&name)
        .await
        .ok_or(StatusCode::NOT_FOUND)?;
    if let Err(message) = check_calibration_allowed(&state, &name, &form).await {
        return Ok(calibrate_error_response(&name, message));
    }
    let result = telescope
        .calibrate(
            form.az_offset_deg.to_radians(),
            form.el_offset_deg.to_radians(),
        )
        .await;
    match result {
        Ok(calibration) => {
            info!(
                "Admin {} ({}) adjusted MD01 pointing for {}: offsets az {:.2}°, el {:.2}°; \
                 position az {} -> {}°, el {} -> {}°",
                admin.name,
                admin.provider,
                name,
                form.az_offset_deg,
                form.el_offset_deg,
                format_deg(calibration.previous.azimuth),
                format_deg(calibration.adjusted.azimuth),
                format_deg(calibration.previous.elevation),
                format_deg(calibration.adjusted.elevation),
            );
            let content = CalibrateResultTemplate {
                previous_az: format_deg(calibration.previous.azimuth),
                previous_el: format_deg(calibration.previous.elevation),
                adjusted_az: format_deg(calibration.adjusted.azimuth),
                adjusted_el: format_deg(calibration.adjusted.elevation),
                name,
                error: None,
            }
            .render()
            .expect("Template rendering should always succeed");
            Ok(Html(content).into_response())
        }
        Err(err) => {
            info!(
                "Admin {} ({}) failed to adjust MD01 pointing for {}: {}",
                admin.name, admin.provider, name, err
            );
            Ok(calibrate_error_response(&name, err.to_string()))
        }
    }
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

/// Drop bookings made by admin users from the usage report.
///
/// Admin status is not a column on the user table — it is a list of user ids
/// in the server config, applied to a `User` when the session loads. So the
/// caller passes that list in rather than this reading anything from the
/// booking rows themselves.
fn without_admin_bookings(bookings: Vec<Booking>, admin_user_ids: &[i64]) -> Vec<Booking> {
    bookings
        .into_iter()
        .filter(|booking| !admin_user_ids.contains(&booking.user_id))
        .collect()
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

    #[test]
    fn admin_bookings_are_dropped_and_others_kept() {
        let rows = vec![
            booking(1, "vale", 0, 3600),
            booking(7, "vale", 3600, 7200),
            booking(2, "brage", 0, 3600),
        ];
        let kept = without_admin_bookings(rows, &[7]);
        assert_eq!(
            kept.iter().map(|b| b.user_id).collect::<Vec<_>>(),
            vec![1, 2]
        );
    }

    #[test]
    fn every_admin_in_the_list_is_dropped() {
        let rows = vec![
            booking(7, "vale", 0, 3600),
            booking(9, "vale", 3600, 7200),
            booking(3, "vale", 7200, 10800),
        ];
        let kept = without_admin_bookings(rows, &[7, 9]);
        assert_eq!(kept.iter().map(|b| b.user_id).collect::<Vec<_>>(), vec![3]);
    }

    /// The unticked box is the default, and it must leave the totals alone.
    #[test]
    fn no_configured_admins_keeps_everything() {
        let rows = vec![booking(1, "vale", 0, 3600), booking(7, "vale", 3600, 7200)];
        assert_eq!(without_admin_bookings(rows, &[]).len(), 2);
    }

    /// Consecutive slots merge into one booking, so dropping an admin run
    /// from the middle has to split the segment count rather than leave the
    /// neighbours looking adjacent.
    #[test]
    fn dropping_an_admin_run_splits_the_surrounding_segment() {
        let rows = vec![
            booking(1, "vale", 0, 3600),
            booking(7, "vale", 3600, 7200),
            booking(1, "vale", 7200, 10800),
        ];
        assert_eq!(count_booking_segments(&rows), 3);
        let kept = without_admin_bookings(rows, &[7]);
        assert_eq!(count_booking_segments(&kept), 2);
    }
}
