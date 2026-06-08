use crate::app::AppState;
use crate::geoip::lookup_country;
use crate::models::booking::Booking;
use crate::models::maintenance::fetch_maintenance_set;
use crate::models::support_announcement::fetch_support_announcement;
use crate::models::user::User;
use crate::routes::index::render_main;
use crate::timefmt::InTz;
use askama::Template;
use axum::extract::{ConnectInfo, Path, Query};
use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::{Extension, Form};
use axum::{
    Router,
    extract::State,
    http::StatusCode,
    routing::{delete, get},
};
use chrono::{DateTime, Datelike, Duration, LocalResult, NaiveDate, Offset, TimeZone, Utc};
use chrono_tz::Tz;
use serde::Deserialize;
use std::net::SocketAddr;

pub fn routes(state: AppState) -> Router {
    Router::new()
        .route("/", get(get_bookings).post(create_booking))
        .route("/export.ics", get(export_bookings_ical))
        .route("/{booking_id}", delete(delete_booking))
        .with_state(state)
}

#[derive(Debug, Clone, PartialEq)]
pub enum SlotStatus {
    Free,
    Mine,
    MineActive,
    OtherUser,
    Past,
    /// A local-time grid cell that maps to no bookable hour because the
    /// user's clocks skipped it on a DST spring-forward day.
    Unavailable,
}

#[derive(Debug, Clone)]
pub struct CalendarSlot {
    pub start_time: DateTime<Utc>,
    pub telescope_name: String,
    pub status: SlotStatus,
    pub booking_id: Option<i64>,
    pub is_current_hour: bool,
    pub booked_by: Option<String>,
}

/// Lay out the calendar grid in the user's local timezone. The bookable
/// unit stays a whole UTC hour (identical for every user, regardless of
/// their zone); this only chooses which (local day, local hour) cell each
/// canonical slot is shown in, and labels rows with local time. `off_min`
/// is the minute component of the zone's UTC offset (0 for whole-hour
/// zones, 30/45 for the half/quarter-hour ones) so the local wall-clock
/// time of every cell lands exactly on a whole UTC hour.
fn build_calendar_slots(
    week_start: NaiveDate,
    telescope_names: &[String],
    bookings: &[Booking],
    user: &User,
    now: DateTime<Utc>,
    tz: Tz,
    off_min: u32,
) -> Vec<Vec<Vec<CalendarSlot>>> {
    let mut result = Vec::new();

    for telescope in telescope_names {
        let mut telescope_days = Vec::new();
        for day_offset in 0..7 {
            let day = week_start + Duration::days(day_offset);
            let mut day_slots = Vec::new();
            for hour in 0..24 {
                // Map this local wall-clock cell back to its UTC instant.
                let naive = day.and_hms_opt(hour, off_min, 0).unwrap();
                let start_time = match tz.from_local_datetime(&naive) {
                    LocalResult::Single(dt) => dt.with_timezone(&Utc),
                    // DST fall-back: two instants share this local time;
                    // show the earliest, the later one is just unreachable
                    // from the grid that day.
                    LocalResult::Ambiguous(dt, _) => dt.with_timezone(&Utc),
                    // DST spring-forward: this local hour doesn't exist.
                    LocalResult::None => {
                        day_slots.push(CalendarSlot {
                            start_time: now,
                            telescope_name: telescope.clone(),
                            status: SlotStatus::Unavailable,
                            booking_id: None,
                            is_current_hour: false,
                            booked_by: None,
                        });
                        continue;
                    }
                };
                let end_time = start_time + Duration::hours(1);

                let is_current_hour = now >= start_time && now < end_time;

                // Find overlapping booking for this telescope+slot
                let overlapping = bookings.iter().find(|b| {
                    b.telescope_name == *telescope
                        && b.start_time < end_time
                        && b.end_time > start_time
                });

                let (status, booking_id, booked_by) = if end_time <= now {
                    (SlotStatus::Past, overlapping.map(|b| b.id), None)
                } else if let Some(b) = overlapping {
                    if b.user_name == user.name && b.user_provider == user.provider {
                        if b.active_at(&now) {
                            (SlotStatus::MineActive, Some(b.id), None)
                        } else {
                            (SlotStatus::Mine, Some(b.id), None)
                        }
                    } else {
                        (SlotStatus::OtherUser, Some(b.id), Some(b.user_name.clone()))
                    }
                } else {
                    (SlotStatus::Free, None, None)
                };

                day_slots.push(CalendarSlot {
                    start_time,
                    telescope_name: telescope.clone(),
                    status,
                    booking_id,
                    is_current_hour,
                    booked_by,
                });
            }
            telescope_days.push(day_slots);
        }
        result.push(telescope_days);
    }

    result
}

fn week_monday(date: NaiveDate) -> NaiveDate {
    date - Duration::days(date.weekday().num_days_from_monday() as i64)
}

#[derive(Deserialize)]
struct WeekQuery {
    week: Option<NaiveDate>,
    user_id: Option<i64>,
}

#[derive(Template)]
#[template(path = "bookings.html")]
struct BookingsTemplate {
    my_bookings: Vec<Booking>,
    telescope_names: Vec<String>,
    maintenance_telescopes: Vec<bool>,
    error: Option<String>,
    now: DateTime<Utc>,
    week_start: NaiveDate,
    week_end: NaiveDate,
    prev_week: String,
    next_week: String,
    days: Vec<NaiveDate>,
    hours: Vec<u32>,
    /// Local wall-clock label for each grid row (e.g. "16:00", or "16:30"
    /// in half-hour zones), indexed by hour 0..24.
    hour_labels: Vec<String>,
    slots: Vec<Vec<Vec<CalendarSlot>>>,
    upcoming_count: usize,
    max_upcoming_bookings: u32,
    at_limit: bool,
    is_admin: bool,
    viewed_user_id: i64,
    all_users: Vec<User>,
    announcement: Option<String>,
    /// Display timezone, used by `.in_tz(tz)` calls in the template.
    tz: Tz,
    /// IANA name (e.g. "Europe/Stockholm") for the help text and JS.
    tz_name: String,
    /// Current zone abbreviation (e.g. "CEST") for column/label headers.
    tz_abbr: String,
}

async fn get_bookings(
    Extension(user): Extension<Option<User>>,
    headers: HeaderMap,
    Query(query): Query<WeekQuery>,
    State(state): State<AppState>,
) -> Result<Response, StatusCode> {
    let Some(user) = user else {
        return Ok(if headers.get("hx-request").is_some() {
            ([("HX-Redirect", "/auth/login")], "").into_response()
        } else {
            Redirect::to("/auth/login").into_response()
        });
    };

    let viewed_user_id = if user.is_admin {
        query.user_id.unwrap_or(user.id)
    } else {
        user.id
    };
    let now = Utc::now();
    let week_start = week_monday(
        query
            .week
            .unwrap_or(now.with_timezone(&user.tz()).date_naive()),
    );
    let content = build_bookings_page(&state, &user, viewed_user_id, now, week_start, None).await?;

    let content = if headers.get("hx-request").is_some() {
        content
    } else {
        render_main(Some(user), content)
    };
    Ok(Html(content).into_response())
}

#[derive(Deserialize, Debug)]
struct SlotBookingForm {
    start_timestamp: i64,
    telescope: String,
    week: Option<NaiveDate>,
    description: Option<String>,
}

async fn create_booking(
    Extension(user): Extension<Option<User>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    State(state): State<AppState>,
    Form(form): Form<SlotBookingForm>,
) -> Result<Response, StatusCode> {
    let Some(user) = user else {
        return Ok(StatusCode::UNAUTHORIZED.into_response());
    };

    let now = Utc::now();
    let start_time =
        DateTime::<Utc>::from_timestamp(form.start_timestamp, 0).ok_or(StatusCode::BAD_REQUEST)?;
    let end_time = start_time + Duration::hours(1);

    if !state.telescopes.contains_key(&form.telescope).await {
        return Ok(StatusCode::BAD_REQUEST.into_response());
    }

    let country = lookup_country(addr.ip());

    let description = form
        .description
        .as_deref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    let maintenance = fetch_maintenance_set(state.database_connection.clone())
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let max_upcoming = state.booking_config.max_upcoming_bookings;
    let upcoming_count = Booking::fetch_for_user(state.database_connection.clone(), &user)
        .await?
        .into_iter()
        .filter(|b| b.end_time > now)
        .count();

    let error = if end_time <= now {
        Some("Cannot book a slot that has already ended.".to_string())
    } else if !user.is_admin && maintenance.contains(&form.telescope) {
        Some(format!(
            "{} is currently under maintenance.",
            form.telescope
        ))
    } else if !user.is_admin && upcoming_count as u32 >= max_upcoming {
        Some(format!(
            "You have reached the maximum of {} upcoming bookings.",
            max_upcoming,
        ))
    } else {
        let inserted = Booking::create(
            state.database_connection.clone(),
            user.clone(),
            form.telescope.clone(),
            start_time,
            end_time,
            description,
            country,
        )
        .await?;
        if inserted {
            None
        } else {
            let local = start_time.with_timezone(&user.tz());
            Some(format!(
                "Slot at {} on {} is already booked.",
                local.format("%H:%M %Z"),
                local.format("%Y-%m-%d"),
            ))
        }
    };

    let week_start = week_monday(
        form.week
            .unwrap_or(now.with_timezone(&user.tz()).date_naive()),
    );
    let content = build_bookings_page(&state, &user, user.id, now, week_start, error).await?;

    let content = if headers.get("hx-request").is_some() {
        content
    } else {
        render_main(Some(user), content)
    };
    Ok(Html(content).into_response())
}

#[derive(Deserialize)]
struct DeleteQuery {
    week: Option<NaiveDate>,
    user_id: Option<i64>,
}

async fn delete_booking(
    Extension(user): Extension<Option<User>>,
    headers: HeaderMap,
    Path(booking_id): Path<i64>,
    Query(query): Query<DeleteQuery>,
    State(state): State<AppState>,
) -> Result<Response, StatusCode> {
    let Some(user) = user else {
        return Err(StatusCode::UNAUTHORIZED);
    };
    let booking = Booking::fetch_one(state.database_connection.clone(), booking_id)
        .await?
        .ok_or(StatusCode::NOT_FOUND)?;
    let success = booking
        .delete(state.database_connection.clone(), &user)
        .await?;
    if !success {
        return Err(StatusCode::UNAUTHORIZED);
    }

    let now = Utc::now();
    let week_start = week_monday(
        query
            .week
            .unwrap_or(now.with_timezone(&user.tz()).date_naive()),
    );
    let viewed_user_id = if user.is_admin {
        query.user_id.unwrap_or(user.id)
    } else {
        user.id
    };
    let content = build_bookings_page(&state, &user, viewed_user_id, now, week_start, None).await?;

    let content = if headers.get("hx-request").is_some() {
        content
    } else {
        render_main(Some(user), content)
    };
    Ok(Html(content).into_response())
}

async fn export_bookings_ical(
    Extension(user): Extension<Option<User>>,
    State(state): State<AppState>,
) -> Result<Response, StatusCode> {
    let Some(user) = user else {
        return Err(StatusCode::UNAUTHORIZED);
    };

    let now = Utc::now();
    let bookings = Booking::fetch_for_user(state.database_connection.clone(), &user)
        .await?
        .into_iter()
        .filter(|b| b.end_time > now)
        .collect::<Vec<_>>();

    let dtstamp = now.format("%Y%m%dT%H%M%SZ");
    let mut ical = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//SALSA//SALSA Telescope//EN\r\nCALSCALE:GREGORIAN\r\nMETHOD:PUBLISH\r\n".to_string();
    for booking in &bookings {
        let mut vevent = format!(
            "BEGIN:VEVENT\r\nUID:salsa-booking-{}@salsa\r\nDTSTAMP:{}\r\nDTSTART:{}\r\nDTEND:{}\r\nSUMMARY:Telescope booking: {}\r\n",
            booking.id,
            dtstamp,
            booking.start_time.format("%Y%m%dT%H%M%SZ"),
            booking.end_time.format("%Y%m%dT%H%M%SZ"),
            booking.telescope_name,
        );
        if let Some(desc) = &booking.description {
            vevent.push_str(&format!("DESCRIPTION:{}\r\n", desc));
        }
        vevent.push_str("END:VEVENT\r\n");
        ical.push_str(&vevent);
    }
    ical.push_str("END:VCALENDAR\r\n");

    Ok((
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/calendar; charset=utf-8"),
            ),
            (
                header::CONTENT_DISPOSITION,
                HeaderValue::from_static("attachment; filename=\"bookings.ics\""),
            ),
        ],
        ical,
    )
        .into_response())
}

async fn build_bookings_page(
    state: &AppState,
    user: &User,
    viewed_user_id: i64,
    now: DateTime<Utc>,
    week_start: NaiveDate,
    error: Option<String>,
) -> Result<String, StatusCode> {
    let tz = user.tz();
    // Minute component of the current UTC offset (0, 30 or 45). The whole
    // grid is laid out at this minute past the local hour so every cell
    // maps to a whole UTC hour.
    let off_min = (now
        .with_timezone(&tz)
        .offset()
        .fix()
        .local_minus_utc()
        .rem_euclid(3600)
        / 60) as u32;
    let tz_name = tz.name().to_string();
    let tz_abbr = now.with_timezone(&tz).format("%Z").to_string();

    let week_end = week_start + Duration::days(6);
    let prev_week = (week_start - Duration::weeks(1))
        .format("%Y-%m-%d")
        .to_string();
    let next_week = (week_start + Duration::weeks(1))
        .format("%Y-%m-%d")
        .to_string();
    let days: Vec<NaiveDate> = (0..7).map(|d| week_start + Duration::days(d)).collect();
    let hours: Vec<u32> = (0..24).collect();
    let hour_labels: Vec<String> = (0..24).map(|h| format!("{h:02}:{off_min:02}")).collect();

    let mut telescope_names = state.telescopes.get_names().await;
    let preferred_order = ["torre", "vale", "brage"];
    telescope_names.sort_by_key(|n| {
        let lower = n.to_lowercase();
        let pos = preferred_order.iter().position(|&p| p == lower.as_str());
        (pos.is_none(), pos.unwrap_or(usize::MAX), lower)
    });
    let maintenance_set = fetch_maintenance_set(state.database_connection.clone())
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let maintenance_telescopes: Vec<bool> = telescope_names
        .iter()
        .map(|name| maintenance_set.contains(name.as_str()))
        .collect();
    let all_bookings = Booking::fetch_all(state.database_connection.clone()).await?;
    let my_bookings: Vec<Booking> =
        Booking::fetch_for_user_id(state.database_connection.clone(), viewed_user_id)
            .await?
            .into_iter()
            .filter(|b| b.end_time > now)
            .collect();
    let all_users = if user.is_admin {
        User::fetch_all_non_guest(state.database_connection.clone())
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    } else {
        vec![]
    };
    let announcement = fetch_support_announcement(state.database_connection.clone())
        .await
        .ok()
        .flatten();

    let slots = build_calendar_slots(
        week_start,
        &telescope_names,
        &all_bookings,
        user,
        now,
        tz,
        off_min,
    );

    let upcoming_count = my_bookings.len();
    let max_upcoming_bookings = state.booking_config.max_upcoming_bookings;
    let at_limit = !user.is_admin
        && viewed_user_id == user.id
        && upcoming_count as u32 >= max_upcoming_bookings;

    let content = BookingsTemplate {
        my_bookings,
        telescope_names,
        maintenance_telescopes,
        error,
        now,
        week_start,
        week_end,
        prev_week,
        next_week,
        days,
        hours,
        hour_labels,
        slots,
        upcoming_count,
        max_upcoming_bookings,
        at_limit,
        is_admin: user.is_admin,
        viewed_user_id,
        all_users,
        announcement,
        tz,
        tz_name,
        tz_abbr,
    }
    .render()
    .expect("Template rendering should always succeed");

    Ok(content)
}
