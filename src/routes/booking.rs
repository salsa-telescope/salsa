use crate::app::AppState;
use crate::models::booking::Booking;
use crate::models::user::User;
use crate::routes::index::render_main;
use askama::Template;
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse, Response};
use axum::{Extension, Form};
use axum::{Router, extract::State, http::StatusCode, routing::get};
use chrono::{DateTime, Duration, NaiveDate, NaiveDateTime, NaiveTime, TimeZone, Utc};
use serde::Deserialize;

pub fn routes(state: AppState) -> Router {
    Router::new()
        .route("/", get(get_bookings).post(create_booking))
        .with_state(state)
}

#[derive(Template)]
#[template(path = "bookings.html")]
struct BookingsTemplate {
    my_bookings: Vec<Booking>,
    bookings: Vec<Booking>,
    telescope_names: Vec<String>,
    error: Option<String>,
    now: DateTime<Utc>,
}

async fn get_bookings(
    Extension(user): Extension<Option<User>>,
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Result<Response, StatusCode> {
    let content = BookingsTemplate {
        my_bookings: Booking::fetch_for_user(
            state.database_connection.clone(),
        user.clone().ok_or(StatusCode::NOT_FOUND)?)
        .await?,
        bookings: Booking::fetch_all(state.database_connection).await?,
        telescope_names: state.telescopes.get_names(),
        error: None,
        now: Utc::now(),
    }
    .render()
    .expect("Template rendering should always succeed");
    let content = if headers.get("hx-request").is_some() {
        content
    } else {
        render_main(user, content)
    };
    Ok(Html(content).into_response())
}

#[derive(Deserialize, Debug)]
struct BookingForm {
    start_date: NaiveDate,
    start_time: NaiveTime,
    telescope: String,
    duration: i64,
}

async fn create_booking(
    Extension(user): Extension<Option<User>>,
    headers: HeaderMap,
    State(state): State<AppState>,
    Form(booking_form): Form<BookingForm>,
) -> Result<Response, StatusCode> {
    let Some(user) = user else {
        return Ok(StatusCode::UNAUTHORIZED.into_response());
    };

    let naive_datetime = NaiveDateTime::new(booking_form.start_date, booking_form.start_time);
    let start_time: DateTime<Utc> = Utc.from_utc_datetime(&naive_datetime);
    let end_time = start_time + Duration::hours(booking_form.duration);

    if !state.telescopes.contains_key(&booking_form.telescope).await {
        return Ok(StatusCode::BAD_REQUEST.into_response());
    }
    let booking = Booking {
        start_time,
        end_time,
        user_name: user.name.clone(),
        user_provider: user.provider.clone(),
        telescope_name: booking_form.telescope,
    };
    // TODO: Do the overlap check in the database instead.
    let bookings = Booking::fetch_all(state.database_connection.clone()).await?;
    let error = if !bookings
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
        Some(format!("It's not possible to book {} at {} for {} minutes. It is already booked.",
            booking.telescope_name,
            booking.start_time,
            (booking.end_time - booking.start_time).num_minutes()
        ))
    };

    let content = BookingsTemplate {
        my_bookings: Booking::fetch_for_user(state.database_connection.clone(), user.clone())
        .await?,
        bookings: Booking::fetch_all(state.database_connection).await?,
        telescope_names: state.telescopes.get_names(),
        error: error,
        now: Utc::now(),
    }
    .render()
    .expect("Template rendering should always succeed");
    let content = if headers.get("hx-request").is_some() {
        content
    } else {
        render_main(Some(user), content)
    };
    Ok(Html(content).into_response())
}
