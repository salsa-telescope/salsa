use crate::app::AppState;
use crate::coords::Location;
use crate::correlator::CorrelatorHandle;
use crate::models::booking::{booking_is_active, consecutive_booking_end};
use crate::models::interferometry::{InterferometrySession, InterferometryVisibility};
use crate::models::telescope_types::{ReceiverConfiguration, TelescopeStatus, TelescopeTarget};
use crate::models::user::User;
use crate::routes::index::render_main;

use askama::Template;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Json, Redirect, Response};
use axum::{Extension, Form, Router, routing::get};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use tracing::error;

pub fn routes(state: AppState) -> Router {
    Router::new()
        .route("/", get(get_list))
        .route("/satellites", get(get_satellites))
        .route("/start", axum::routing::post(post_start))
        .route("/stop", axum::routing::post(post_stop))
        .route("/telescope-status", get(get_telescope_status))
        .route("/track/{telescope_id}", axum::routing::post(post_track))
        .route(
            "/stop-tel/{telescope_id}",
            axum::routing::post(post_stop_telescope),
        )
        .route("/{session_id}", get(get_session).delete(delete_session))
        .route("/{session_id}/data", get(get_session_data))
        .with_state(state)
}

// ---------------------------------------------------------------------------
// List page — shows active bookings and past sessions
// ---------------------------------------------------------------------------

#[derive(Template)]
#[template(path = "interferometry_list.html")]
struct ListTemplate {
    active_telescopes: Vec<String>,
    running_session_id: Option<i64>,
    telescope_names: Vec<String>,
}

fn session_target_label(s: &InterferometrySession, state: &AppState) -> String {
    let sat_name = if s.coordinate_system == "gnss" {
        state.tle_cache.satellite_name(s.target_x as u64)
    } else {
        None
    };
    s.target_label(sat_name)
}

async fn get_list(
    State(state): State<AppState>,
    Extension(user): Extension<Option<User>>,
) -> Response {
    let Some(user) = user else {
        return Redirect::to("/auth/login").into_response();
    };

    // Telescopes the user currently has active bookings for
    let active_telescopes = match crate::models::booking::Booking::fetch_for_user(
        state.database_connection.clone(),
        &user,
    )
    .await
    {
        Ok(bookings) => {
            let now = Utc::now();
            bookings
                .into_iter()
                .filter(|b| b.active_at(&now))
                .map(|b| b.telescope_name)
                .collect::<Vec<_>>()
        }
        Err(e) => {
            error!("interferometry list: {e:?}");
            vec![]
        }
    };

    let running_session_id = state
        .active_correlator
        .lock()
        .await
        .as_ref()
        .map(|c| c.session_id);

    let telescope_names = state.telescopes.get_names().await;

    let content = ListTemplate {
        active_telescopes,
        running_session_id,
        telescope_names,
    }
    .render()
    .expect("template ok");

    Html(render_main(Some(user), content)).into_response()
}

// ---------------------------------------------------------------------------
// Satellites JSON (same format as /observe/{id}/satellites)
// ---------------------------------------------------------------------------

async fn get_satellites(State(state): State<AppState>) -> impl IntoResponse {
    let location = match state.telescopes.get_all().await.into_iter().next() {
        Some(tel) => tel
            .get_info()
            .await
            .map(|i| i.location)
            .unwrap_or(Location {
                longitude: 0.0,
                latitude: 0.0,
            }),
        None => Location {
            longitude: 0.0,
            latitude: 0.0,
        },
    };
    let satellites = state.tle_cache.visible_satellites(location, Utc::now());
    let json: Vec<_> = satellites
        .iter()
        .map(|s| {
            serde_json::json!({
                "norad_id": s.norad_id,
                "name": s.name,
                "elevation_deg": s.direction.elevation.to_degrees(),
                "azimuth_deg": s.direction.azimuth.to_degrees(),
                "freq_mhz": s.freq_mhz,
            })
        })
        .collect();
    Json(json)
}

// ---------------------------------------------------------------------------
// Telescope status JSON (polled by the list page)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct TelescopeStatusQuery {
    a: Option<String>,
    b: Option<String>,
}

#[derive(Serialize)]
struct TelStatusEntry {
    status: String,
    booking_end_ms: Option<i64>,
    az_deg: Option<f64>,
    el_deg: Option<f64>,
    target_az_deg: Option<f64>,
    target_el_deg: Option<f64>,
}

#[derive(Serialize)]
struct TelescopeStatusResponse {
    a: Option<TelStatusEntry>,
    b: Option<TelStatusEntry>,
}

async fn tel_status_entry(state: &AppState, user: &User, tel_id: &str) -> Option<TelStatusEntry> {
    let tel = state.telescopes.get(tel_id).await?;
    let info = tel.get_info().await.ok()?;
    let status = match info.status {
        TelescopeStatus::Idle => "Idle",
        TelescopeStatus::Slewing => "Slewing",
        TelescopeStatus::Tracking => "Tracking",
    }
    .to_string();
    let booking_end_ms = consecutive_booking_end(state.database_connection.clone(), user, tel_id)
        .await
        .ok()
        .flatten()
        .map(|t| t.timestamp_millis());
    let az_deg = info.current_horizontal.map(|h| h.azimuth.to_degrees());
    let el_deg = info.current_horizontal.map(|h| h.elevation.to_degrees());
    let target_az_deg = info.commanded_horizontal.map(|h| h.azimuth.to_degrees());
    let target_el_deg = info.commanded_horizontal.map(|h| h.elevation.to_degrees());
    Some(TelStatusEntry {
        status,
        booking_end_ms,
        az_deg,
        el_deg,
        target_az_deg,
        target_el_deg,
    })
}

async fn get_telescope_status(
    State(state): State<AppState>,
    Extension(user): Extension<Option<User>>,
    Query(query): Query<TelescopeStatusQuery>,
) -> Response {
    let Some(user) = user else {
        return StatusCode::UNAUTHORIZED.into_response();
    };

    let a = match query.a.as_deref() {
        Some(id) => tel_status_entry(&state, &user, id).await,
        None => None,
    };
    let b = match query.b.as_deref() {
        Some(id) => tel_status_entry(&state, &user, id).await,
        None => None,
    };

    Json(TelescopeStatusResponse { a, b }).into_response()
}

// ---------------------------------------------------------------------------
// Track telescope
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct TrackForm {
    pub coordinate_system: String,
    #[serde(default)]
    pub target_x: f64,
    #[serde(default)]
    pub target_y: f64,
}

async fn post_track(
    State(state): State<AppState>,
    Extension(user): Extension<Option<User>>,
    Path(telescope_id): Path<String>,
    Form(form): Form<TrackForm>,
) -> Response {
    let Some(user) = user else {
        return Redirect::to("/auth/login").into_response();
    };

    match booking_is_active(state.database_connection.clone(), &user, &telescope_id).await {
        Ok(true) => {}
        Ok(false) => return StatusCode::UNAUTHORIZED.into_response(),
        Err(e) => {
            error!("booking_is_active: {e:?}");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    }

    let tel = match state.telescopes.get(&telescope_id).await {
        Some(t) => t,
        None => return StatusCode::NOT_FOUND.into_response(),
    };

    let target = match form.coordinate_system.as_str() {
        "sun" => TelescopeTarget::Sun,
        "galactic" => TelescopeTarget::Galactic {
            longitude: form.target_x.to_radians(),
            latitude: form.target_y.to_radians(),
        },
        "equatorial" => TelescopeTarget::Equatorial {
            right_ascension: form.target_x.to_radians(),
            declination: form.target_y.to_radians(),
        },
        "horizontal" => TelescopeTarget::Horizontal {
            azimuth: form.target_x.to_radians(),
            elevation: form.target_y.to_radians(),
        },
        "gnss" => TelescopeTarget::Satellite {
            norad_id: form.target_x as u64,
        },
        "stow" => {
            let info = match tel.get_info().await {
                Ok(i) => i,
                Err(e) => {
                    error!("get_info {telescope_id}: {e:?}");
                    return StatusCode::INTERNAL_SERVER_ERROR.into_response();
                }
            };
            match info.stow_position {
                Some(pos) => TelescopeTarget::Horizontal {
                    azimuth: pos.azimuth,
                    elevation: pos.elevation,
                },
                None => {
                    error!("No stow position configured for {telescope_id}");
                    return StatusCode::NOT_FOUND.into_response();
                }
            }
        }
        _ => return StatusCode::BAD_REQUEST.into_response(),
    };

    if let Err(e) = tel.set_target(target, 0.0, 0.0).await {
        error!("set_target {telescope_id}: {e:?}");
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }

    Redirect::to("/interferometry").into_response()
}

// ---------------------------------------------------------------------------
// Stop telescope (pointing only, not the correlator)
// ---------------------------------------------------------------------------

async fn post_stop_telescope(
    State(state): State<AppState>,
    Extension(user): Extension<Option<User>>,
    Path(telescope_id): Path<String>,
) -> Response {
    let Some(user) = user else {
        return Redirect::to("/auth/login").into_response();
    };

    match booking_is_active(state.database_connection.clone(), &user, &telescope_id).await {
        Ok(true) => {}
        Ok(false) => return StatusCode::UNAUTHORIZED.into_response(),
        Err(e) => {
            error!("booking_is_active: {e:?}");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    }

    let tel = match state.telescopes.get(&telescope_id).await {
        Some(t) => t,
        None => return StatusCode::NOT_FOUND.into_response(),
    };

    if let Err(e) = tel.stop().await {
        error!("stop telescope {telescope_id}: {e:?}");
    }

    Redirect::to("/interferometry").into_response()
}

// ---------------------------------------------------------------------------
// Start correlator session
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct StartForm {
    pub telescope_a: String,
    pub telescope_b: String,
    pub coordinate_system: String,
    #[serde(default)]
    pub target_x: f64,
    #[serde(default)]
    pub target_y: f64,
    pub center_freq_mhz: f64,
    pub bandwidth_mhz: f64,
    pub spectral_channels: usize,
}

async fn post_start(
    State(state): State<AppState>,
    Extension(user): Extension<Option<User>>,
    Form(form): Form<StartForm>,
) -> Response {
    let Some(user) = user else {
        return Redirect::to("/auth/login").into_response();
    };

    if form.telescope_a == form.telescope_b {
        return (StatusCode::BAD_REQUEST, "Telescopes must be different").into_response();
    }

    // Require both telescopes to be tracking
    for tel_id in [&form.telescope_a, &form.telescope_b] {
        let Some(tel) = state.telescopes.get(tel_id).await else {
            return (
                StatusCode::BAD_REQUEST,
                format!("Telescope {tel_id} not found"),
            )
                .into_response();
        };
        match tel.get_info().await {
            Ok(info) if info.status == TelescopeStatus::Tracking => {}
            Ok(_) => {
                return (
                    StatusCode::BAD_REQUEST,
                    format!("Telescope {tel_id} is not tracking — point both telescopes first"),
                )
                    .into_response();
            }
            Err(e) => {
                error!("get_info {tel_id}: {e:?}");
                return StatusCode::INTERNAL_SERVER_ERROR.into_response();
            }
        }
    }

    let center_freq_hz = form.center_freq_mhz * 1e6;
    let bandwidth_hz = form.bandwidth_mhz * 1e6;

    // Stop any running correlator first
    {
        let mut guard = state.active_correlator.lock().await;
        if let Some(mut old) = guard.take() {
            let _ =
                InterferometrySession::finalize(state.database_connection.clone(), old.session_id)
                    .await;
            old.stop().await;
        }
    }

    let tel_a = match state.telescopes.get(&form.telescope_a).await {
        Some(t) => t,
        None => {
            return (StatusCode::BAD_REQUEST, "Telescope A not found").into_response();
        }
    };
    let tel_b = match state.telescopes.get(&form.telescope_b).await {
        Some(t) => t,
        None => {
            return (StatusCode::BAD_REQUEST, "Telescope B not found").into_response();
        }
    };

    let config = ReceiverConfiguration {
        integrate: true,
        center_freq_hz,
        bandwidth_hz,
        spectral_channels: form.spectral_channels,
        ..Default::default()
    };

    let rx_a = match tel_a.start_iq_stream(config).await {
        Ok(rx) => rx,
        Err(e) => {
            error!("start_iq_stream A: {e:?}");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to start IQ stream for telescope A",
            )
                .into_response();
        }
    };
    let rx_b = match tel_b.start_iq_stream(config).await {
        Ok(rx) => rx,
        Err(e) => {
            // Clean up A
            let _ = tel_a
                .set_receiver_configuration(ReceiverConfiguration {
                    integrate: false,
                    ..Default::default()
                })
                .await;
            error!("start_iq_stream B: {e:?}");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to start IQ stream for telescope B",
            )
                .into_response();
        }
    };

    let session_id = match InterferometrySession::create(
        state.database_connection.clone(),
        &user,
        form.telescope_a.clone(),
        form.telescope_b.clone(),
        form.coordinate_system.clone(),
        form.target_x,
        form.target_y,
        center_freq_hz,
        bandwidth_hz,
    )
    .await
    {
        Ok(id) => id,
        Err(e) => {
            error!("create session: {e:?}");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to create session",
            )
                .into_response();
        }
    };

    let handle = CorrelatorHandle::start(
        session_id,
        form.telescope_a.clone(),
        form.telescope_b.clone(),
        rx_a,
        rx_b,
        config,
        state.database_connection.clone(),
    );

    *state.active_correlator.lock().await = Some(handle);

    Redirect::to(&format!("/interferometry/{session_id}")).into_response()
}

// ---------------------------------------------------------------------------
// Stop correlator session
// ---------------------------------------------------------------------------

async fn post_stop(
    State(state): State<AppState>,
    Extension(user): Extension<Option<User>>,
) -> Response {
    let Some(_user) = user else {
        return Redirect::to("/auth/login").into_response();
    };

    let mut guard = state.active_correlator.lock().await;
    if let Some(mut handle) = guard.take() {
        let session_id = handle.session_id;
        let _ =
            InterferometrySession::finalize(state.database_connection.clone(), session_id).await;
        handle.stop().await;
        return Redirect::to(&format!("/interferometry/{session_id}")).into_response();
    }

    Redirect::to("/interferometry").into_response()
}

// ---------------------------------------------------------------------------
// Session detail page
// ---------------------------------------------------------------------------

#[derive(Template)]
#[template(path = "interferometry_session.html")]
struct SessionTemplate {
    session: InterferometrySession,
    target_label: String,
    is_running: bool,
    visibility_count: usize,
    center_freq_mhz: f64,
    half_bw_mhz: f64,
}

async fn get_session(
    State(state): State<AppState>,
    Extension(user): Extension<Option<User>>,
    Path(session_id): Path<i64>,
) -> Response {
    let Some(user) = user else {
        return Redirect::to("/auth/login").into_response();
    };

    let session = match InterferometrySession::fetch_one(
        state.database_connection.clone(),
        session_id,
        Some(user.id),
    )
    .await
    {
        Ok(Some(s)) => s,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(e) => {
            error!("fetch session: {e:?}");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    let visibilities = match InterferometryVisibility::fetch_for_session(
        state.database_connection.clone(),
        session_id,
    )
    .await
    {
        Ok(v) => v,
        Err(e) => {
            error!("fetch visibilities: {e:?}");
            vec![]
        }
    };

    let is_running = state
        .active_correlator
        .lock()
        .await
        .as_ref()
        .is_some_and(|c| c.session_id == session_id);

    let center_freq_mhz = session.center_freq_hz / 1e6;
    let half_bw_mhz = session.bandwidth_hz / 2e6;
    let target_label = session_target_label(&session, &state);
    let content = SessionTemplate {
        session,
        target_label,
        is_running,
        visibility_count: visibilities.len(),
        center_freq_mhz,
        half_bw_mhz,
    }
    .render()
    .expect("template ok");

    Html(render_main(Some(user), content)).into_response()
}

// ---------------------------------------------------------------------------
// JSON data endpoint (polled by the session page charts)
// ---------------------------------------------------------------------------

async fn get_session_data(
    State(state): State<AppState>,
    Extension(user): Extension<Option<User>>,
    Path(session_id): Path<i64>,
) -> Response {
    let Some(user) = user else {
        return StatusCode::UNAUTHORIZED.into_response();
    };

    // Verify ownership
    match InterferometrySession::fetch_one(
        state.database_connection.clone(),
        session_id,
        Some(user.id),
    )
    .await
    {
        Ok(Some(_)) => {}
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(e) => {
            error!("fetch session: {e:?}");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    }

    match InterferometryVisibility::fetch_for_session(state.database_connection.clone(), session_id)
        .await
    {
        Ok(visibilities) => Json(visibilities).into_response(),
        Err(e) => {
            error!("fetch visibilities: {e:?}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

// ---------------------------------------------------------------------------
// Delete session
// ---------------------------------------------------------------------------

async fn delete_session(
    State(state): State<AppState>,
    Extension(user): Extension<Option<User>>,
    Path(session_id): Path<i64>,
) -> Response {
    let Some(user) = user else {
        return StatusCode::UNAUTHORIZED.into_response();
    };

    // Refuse to delete a currently running session
    let is_running = state
        .active_correlator
        .lock()
        .await
        .as_ref()
        .is_some_and(|c| c.session_id == session_id);
    if is_running {
        return (
            StatusCode::CONFLICT,
            "Session is still running — stop it first",
        )
            .into_response();
    }

    match InterferometrySession::delete(state.database_connection.clone(), session_id, &user).await
    {
        Ok(true) => Redirect::to("/observations?mode=interferometry").into_response(),
        Ok(false) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => {
            error!("delete session: {e:?}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}
