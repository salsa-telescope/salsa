use crate::app::AppState;
use crate::models::booking::Booking;
use crate::models::user::User;
use crate::routes::index::render_main;
use askama::Template;
use axum::extract::{Path, Query};
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse, Response};
use axum::{Extension, Form};
use axum::{
    Router,
    extract::State,
    http::StatusCode,
    routing::{delete, get},
};
use chrono::{DateTime, Datelike, Duration, NaiveDate, Utc};
use serde::Deserialize;

pub fn routes(state: AppState) -> Router {
    Router::new()
        .route("/", get(get_bookings).post(create_booking))
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
}

#[derive(Debug, Clone)]
pub struct CalendarSlot {
    pub start_time: DateTime<Utc>,
    pub telescope_name: String,
    pub status: SlotStatus,
    pub booking_id: Option<i64>,
    pub is_current_hour: bool,
}

fn build_calendar_slots(
    week_start: NaiveDate,
    telescope_names: &[String],
    bookings: &[Booking],
    user: &User,
    now: DateTime<Utc>,
) -> Vec<Vec<Vec<CalendarSlot>>> {
    let mut result = Vec::new();

    for telescope in telescope_names {
        let mut telescope_days = Vec::new();
        for day_offset in 0..7 {
            let day = week_start + Duration::days(day_offset);
            let mut day_slots = Vec::new();
            for hour in 0..24 {
                let start_time = day.and_hms_opt(hour, 0, 0).unwrap().and_utc();
                let end_time = start_time + Duration::hours(1);

                let is_current_hour = now >= start_time && now < end_time;

                // Find overlapping booking for this telescope+slot
                let overlapping = bookings.iter().find(|b| {
                    b.telescope_name == *telescope
                        && b.start_time < end_time
                        && b.end_time > start_time
                });

                let (status, booking_id) = if end_time <= now {
                    (SlotStatus::Past, overlapping.map(|b| b.id))
                } else if let Some(b) = overlapping {
                    if b.user_name == user.name && b.user_provider == user.provider {
                        if b.active_at(&now) {
                            (SlotStatus::MineActive, Some(b.id))
                        } else {
                            (SlotStatus::Mine, Some(b.id))
                        }
                    } else {
                        (SlotStatus::OtherUser, None)
                    }
                } else {
                    (SlotStatus::Free, None)
                };

                day_slots.push(CalendarSlot {
                    start_time,
                    telescope_name: telescope.clone(),
                    status,
                    booking_id,
                    is_current_hour,
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
}

#[derive(Template)]
#[template(path = "bookings.html")]
struct BookingsTemplate {
    my_bookings: Vec<Booking>,
    telescope_names: Vec<String>,
    error: Option<String>,
    now: DateTime<Utc>,
    week_start: NaiveDate,
    week_end: NaiveDate,
    prev_week: String,
    next_week: String,
    days: Vec<NaiveDate>,
    hours: Vec<u32>,
    slots: Vec<Vec<Vec<CalendarSlot>>>,
    upcoming_count: usize,
    max_upcoming_bookings: u32,
    at_limit: bool,
}

async fn get_bookings(
    Extension(user): Extension<Option<User>>,
    headers: HeaderMap,
    Query(query): Query<WeekQuery>,
    State(state): State<AppState>,
) -> Result<Response, StatusCode> {
    let Some(user) = user else {
        return Ok(StatusCode::UNAUTHORIZED.into_response());
    };

    let now = Utc::now();
    let week_start = week_monday(query.week.unwrap_or(now.date_naive()));
    let content = build_bookings_page(&state, &user, now, week_start, None).await?;

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
}

async fn create_booking(
    Extension(user): Extension<Option<User>>,
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

    let booking = Booking {
        id: -1,
        start_time,
        end_time,
        user_name: user.name.clone(),
        user_provider: user.provider.clone(),
        telescope_name: form.telescope.clone(),
    };

    let bookings = Booking::fetch_all(state.database_connection.clone()).await?;
    let max_upcoming = state.booking_config.max_upcoming_bookings;
    let upcoming_count = Booking::fetch_for_user(state.database_connection.clone(), &user)
        .await?
        .into_iter()
        .filter(|b| b.end_time > now)
        .count();

    let error = if upcoming_count as u32 >= max_upcoming {
        Some(format!(
            "You have reached the maximum of {} upcoming bookings.",
            max_upcoming,
        ))
    } else if !bookings
        .iter()
        .filter(|b| b.telescope_name == booking.telescope_name && b.overlaps(&booking))
        .any(|_| true)
    {
        Booking::create(
            state.database_connection.clone(),
            user.clone(),
            booking.telescope_name,
            booking.start_time,
            booking.end_time,
        )
        .await?;
        None
    } else {
        Some(format!(
            "Slot at {} on {} is already booked.",
            start_time.format("%H:%M"),
            start_time.format("%Y-%m-%d"),
        ))
    };

    let week_start = week_monday(form.week.unwrap_or(now.date_naive()));
    let content = build_bookings_page(&state, &user, now, week_start, error).await?;

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
    let week_start = week_monday(query.week.unwrap_or(now.date_naive()));
    let content = build_bookings_page(&state, &user, now, week_start, None).await?;

    let content = if headers.get("hx-request").is_some() {
        content
    } else {
        render_main(Some(user), content)
    };
    Ok(Html(content).into_response())
}

async fn build_bookings_page(
    state: &AppState,
    user: &User,
    now: DateTime<Utc>,
    week_start: NaiveDate,
    error: Option<String>,
) -> Result<String, StatusCode> {
    let week_end = week_start + Duration::days(6);
    let prev_week = (week_start - Duration::weeks(1))
        .format("%Y-%m-%d")
        .to_string();
    let next_week = (week_start + Duration::weeks(1))
        .format("%Y-%m-%d")
        .to_string();
    let days: Vec<NaiveDate> = (0..7).map(|d| week_start + Duration::days(d)).collect();
    let hours: Vec<u32> = (0..24).collect();

    let telescope_names = state.telescopes.get_names().await;
    let all_bookings = Booking::fetch_all(state.database_connection.clone()).await?;
    let my_bookings: Vec<Booking> =
        Booking::fetch_for_user(state.database_connection.clone(), user)
            .await?
            .into_iter()
            .filter(|b| b.end_time > now)
            .collect();

    let slots = build_calendar_slots(week_start, &telescope_names, &all_bookings, user, now);

    let upcoming_count = my_bookings.len();
    let max_upcoming_bookings = state.booking_config.max_upcoming_bookings;
    let at_limit = upcoming_count as u32 >= max_upcoming_bookings;

    let content = BookingsTemplate {
        my_bookings,
        telescope_names,
        error,
        now,
        week_start,
        week_end,
        prev_week,
        next_week,
        days,
        hours,
        slots,
        upcoming_count,
        max_upcoming_bookings,
        at_limit,
    }
    .render()
    .expect("Template rendering should always succeed");

    Ok(content)
}
