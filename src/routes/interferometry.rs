use crate::app::AppState;
use crate::coords::Location;
use crate::correlator::CorrelatorHandle;
use crate::models::booking::{booking_is_active, consecutive_booking_end};
use crate::models::interferometry::{InterferometrySession, InterferometryVisibility};
use crate::models::telescope_types::{ReceiverConfiguration, TelescopeStatus, TelescopeTarget};
use crate::models::user::User;
use crate::routes::index::render_main;
use crate::routes::observe::{
    FREQ_MAX_ADMIN_MHZ, FREQ_MAX_USER_MHZ, FREQ_MIN_ADMIN_MHZ, FREQ_MIN_USER_MHZ,
};

use askama::Template;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Json, Redirect, Response};
use axum::{Extension, Form, Router, routing::get};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use tracing::error;

/// Finalize the session row, stop the correlator task, and release both telescopes'
/// IQ streams. Must be called for every path that ends a correlator session —
/// otherwise the telescopes' receiver state stays stuck and the next session can't
/// start (FakeTelescope: `iq_cancellation_token` stays `Some`;
/// SalsaTelescope: `integrate` flag stays `true`).
pub async fn stop_correlator_session(state: &AppState, mut handle: CorrelatorHandle) {
    let session_id = handle.session_id;
    let tel_a = handle.telescope_a.clone();
    let tel_b = handle.telescope_b.clone();

    // Stop the correlator task before finalising the session so no visibility
    // rows can land with a timestamp after the session's end_time.
    handle.stop().await;
    if let Err(e) =
        InterferometrySession::finalize(state.database_connection.clone(), session_id).await
    {
        error!("finalize session {session_id}: {e:?}");
    }
    release_iq_stream(state, &tel_a).await;
    release_iq_stream(state, &tel_b).await;
}

async fn release_iq_stream(state: &AppState, telescope_id: &str) {
    let Some(tel) = state.telescopes.get(telescope_id).await else {
        return;
    };
    let reset = ReceiverConfiguration {
        integrate: false,
        ..Default::default()
    };
    if let Err(e) = tel.set_receiver_configuration(reset).await {
        error!("release IQ stream for {telescope_id}: {e:?}");
    }
}

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
    freq_min_mhz: u32,
    freq_max_mhz: u32,
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

    let (freq_min_mhz, freq_max_mhz) = if user.is_admin {
        (FREQ_MIN_ADMIN_MHZ, FREQ_MAX_ADMIN_MHZ)
    } else {
        (FREQ_MIN_USER_MHZ, FREQ_MAX_USER_MHZ)
    };

    let content = ListTemplate {
        active_telescopes,
        running_session_id,
        telescope_names,
        freq_min_mhz,
        freq_max_mhz,
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
        "gnss" => {
            // `target_x` is the NORAD id; `0` is the serde default and also not
            // a real satellite, so reject it explicitly rather than pointing the
            // telescope at "NORAD 0".
            if form.target_x < 1.0 {
                return (StatusCode::BAD_REQUEST, "Please pick a satellite").into_response();
            }
            TelescopeTarget::Satellite {
                norad_id: form.target_x as u64,
            }
        }
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
    pub center_freq_mhz: f64,
    pub bandwidth_mhz: f64,
    pub spectral_channels: usize,
}

/// Allowed IQ sample rates (MHz). The USRP N210 exposes this list and the UI
/// offers the same. Keep in sync with the `<select>` in `interferometry_list.html`.
const ALLOWED_BANDWIDTHS_MHZ: &[f64] = &[1.0, 2.5];
const MIN_SPECTRAL_CHANNELS: usize = 1;
const MAX_SPECTRAL_CHANNELS: usize = 1024;

/// Convert the telescopes' matched `current_target` into the `(coordinate_system,
/// target_x, target_y)` triple we store on the session row. The telescope tracker
/// only hands back values from the `TelescopeTarget` enum, so the string side of
/// the storage is restricted to a known allowlist — unlike an uninspected form
/// field, which could be any text.
fn target_to_session_fields(target: TelescopeTarget) -> (String, f64, f64) {
    match target {
        TelescopeTarget::Equatorial {
            right_ascension,
            declination,
        } => (
            "equatorial".into(),
            right_ascension.to_degrees(),
            declination.to_degrees(),
        ),
        TelescopeTarget::Galactic {
            longitude,
            latitude,
        } => (
            "galactic".into(),
            longitude.to_degrees(),
            latitude.to_degrees(),
        ),
        TelescopeTarget::Horizontal { azimuth, elevation } => (
            "horizontal".into(),
            azimuth.to_degrees(),
            elevation.to_degrees(),
        ),
        TelescopeTarget::Sun => ("sun".into(), 0.0, 0.0),
        TelescopeTarget::Satellite { norad_id } => ("gnss".into(), norad_id as f64, 0.0),
    }
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

    // Collect each telescope's info in one pass; we need the target in step two.
    let mut infos = Vec::with_capacity(2);
    for tel_id in [&form.telescope_a, &form.telescope_b] {
        match booking_is_active(state.database_connection.clone(), &user, tel_id).await {
            Ok(true) => {}
            Ok(false) => {
                return (
                    StatusCode::FORBIDDEN,
                    format!("No active booking for telescope {tel_id}"),
                )
                    .into_response();
            }
            Err(e) => {
                error!("booking_is_active {tel_id}: {e:?}");
                return StatusCode::INTERNAL_SERVER_ERROR.into_response();
            }
        }

        let Some(tel) = state.telescopes.get(tel_id).await else {
            return (
                StatusCode::BAD_REQUEST,
                format!("Telescope {tel_id} not found"),
            )
                .into_response();
        };
        match tel.get_info().await {
            Ok(info) if info.status == TelescopeStatus::Tracking => infos.push(info),
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

    // Both must be pointing at the same target — otherwise cross-correlating
    // them is meaningless. Deriving the session's target from live telescope
    // state (rather than the form) makes it impossible for the stored
    // coordinate_system/target_x/target_y to disagree with reality, and keeps
    // `coordinate_system` restricted to the known enum variants.
    let target_a = match infos[0].current_target {
        Some(t) => t,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                "Telescope A has no target set — track something first",
            )
                .into_response();
        }
    };
    let target_b = match infos[1].current_target {
        Some(t) => t,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                "Telescope B has no target set — track something first",
            )
                .into_response();
        }
    };
    if target_a != target_b {
        return (
            StatusCode::BAD_REQUEST,
            "Telescopes must be pointing at the same target",
        )
            .into_response();
    }
    let (coordinate_system, target_x, target_y) = target_to_session_fields(target_a);

    let (freq_min, freq_max) = if user.is_admin {
        (FREQ_MIN_ADMIN_MHZ, FREQ_MAX_ADMIN_MHZ)
    } else {
        (FREQ_MIN_USER_MHZ, FREQ_MAX_USER_MHZ)
    };
    if form.center_freq_mhz < freq_min as f64 || form.center_freq_mhz > freq_max as f64 {
        return (
            StatusCode::BAD_REQUEST,
            format!("Center frequency must be between {freq_min} and {freq_max} MHz"),
        )
            .into_response();
    }
    if !ALLOWED_BANDWIDTHS_MHZ.contains(&form.bandwidth_mhz) {
        return (
            StatusCode::BAD_REQUEST,
            format!("Bandwidth must be one of {:?} MHz", ALLOWED_BANDWIDTHS_MHZ),
        )
            .into_response();
    }
    if form.spectral_channels < MIN_SPECTRAL_CHANNELS
        || form.spectral_channels > MAX_SPECTRAL_CHANNELS
    {
        return (
            StatusCode::BAD_REQUEST,
            format!(
                "Spectral channels must be between {MIN_SPECTRAL_CHANNELS} and {MAX_SPECTRAL_CHANNELS}"
            ),
        )
            .into_response();
    }

    let center_freq_hz = form.center_freq_mhz * 1e6;
    let bandwidth_hz = form.bandwidth_mhz * 1e6;

    // Stop any running correlator first
    let previous = state.active_correlator.lock().await.take();
    if let Some(old) = previous {
        stop_correlator_session(&state, old).await;
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
            release_iq_stream(&state, &form.telescope_a).await;
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
        coordinate_system,
        target_x,
        target_y,
        center_freq_hz,
        bandwidth_hz,
    )
    .await
    {
        Ok(id) => id,
        Err(e) => {
            error!("create session: {e:?}");
            release_iq_stream(&state, &form.telescope_a).await;
            release_iq_stream(&state, &form.telescope_b).await;
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
    let Some(user) = user else {
        return Redirect::to("/auth/login").into_response();
    };

    // Only the session owner (or an admin) may stop a running session — otherwise
    // any logged-in user could cancel someone else's observation.
    let running_session_id = state
        .active_correlator
        .lock()
        .await
        .as_ref()
        .map(|c| c.session_id);
    let Some(session_id) = running_session_id else {
        return Redirect::to("/interferometry").into_response();
    };
    match InterferometrySession::fetch_one(state.database_connection.clone(), session_id, None)
        .await
    {
        Ok(Some(session)) if user.is_admin || session.user_id == user.id => {}
        Ok(_) => return StatusCode::FORBIDDEN.into_response(),
        Err(e) => {
            error!("fetch session for stop: {e:?}");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    }

    // Re-take under the lock. If the running session changed between the
    // ownership check and here, put the handle back and redirect — we only
    // authorised to stop `session_id`, not whatever is running now.
    let mut guard = state.active_correlator.lock().await;
    match guard.take() {
        Some(handle) if handle.session_id == session_id => {
            drop(guard);
            stop_correlator_session(&state, handle).await;
            Redirect::to(&format!("/interferometry/{session_id}?from=controls")).into_response()
        }
        Some(other) => {
            *guard = Some(other);
            Redirect::to("/interferometry").into_response()
        }
        None => Redirect::to("/interferometry").into_response(),
    }
}

// ---------------------------------------------------------------------------
// Session detail page
// ---------------------------------------------------------------------------

/// Bound on how many visibility rows the `/data` endpoint returns per request.
/// Shared between the route handler and the JS drain loop in the session
/// template — do not change one without the other.
const MAX_VISIBILITY_ROWS_PER_REQUEST: i64 = 1800;

#[derive(Template)]
#[template(path = "interferometry_session.html")]
struct SessionTemplate {
    session: InterferometrySession,
    target_label: String,
    is_running: bool,
    visibility_count: usize,
    center_freq_mhz: f64,
    half_bw_mhz: f64,
    back_url: String,
    back_label: String,
    max_rows_per_response: i64,
}

#[derive(Deserialize)]
struct SessionQuery {
    from: Option<String>,
}

async fn get_session(
    State(state): State<AppState>,
    Extension(user): Extension<Option<User>>,
    Path(session_id): Path<i64>,
    Query(query): Query<SessionQuery>,
) -> Response {
    let Some(user) = user else {
        return Redirect::to("/auth/login").into_response();
    };

    let user_id_filter = if user.is_admin { None } else { Some(user.id) };
    let session = match InterferometrySession::fetch_one(
        state.database_connection.clone(),
        session_id,
        user_id_filter,
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

    let visibility_count = match InterferometryVisibility::count_for_session(
        state.database_connection.clone(),
        session_id,
    )
    .await
    {
        Ok(n) => n as usize,
        Err(e) => {
            error!("count visibilities: {e:?}");
            0
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
    let target_label = session.target_label_from_cache(&state.tle_cache);
    let (back_url, back_label) = if query.from.as_deref() == Some("controls") {
        ("/interferometry".to_string(), "Interferometry".to_string())
    } else {
        (
            "/observations?mode=interferometry".to_string(),
            "Observation archive".to_string(),
        )
    };
    let content = SessionTemplate {
        session,
        target_label,
        is_running,
        visibility_count,
        center_freq_mhz,
        half_bw_mhz,
        back_url,
        back_label,
        max_rows_per_response: MAX_VISIBILITY_ROWS_PER_REQUEST,
    }
    .render()
    .expect("template ok");

    Html(render_main(Some(user), content)).into_response()
}

// ---------------------------------------------------------------------------
// JSON data endpoint (polled by the session page charts)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct DataQuery {
    #[serde(default)]
    after_id: i64,
}

#[derive(Serialize)]
struct SessionDataResponse {
    rows: Vec<InterferometryVisibility>,
    /// True only while this session is the one held by `active_correlator` at
    /// the moment the request is handled. The client uses this to clear the
    /// "● Running" badge without a page refresh once the session ends.
    running: bool,
}

async fn get_session_data(
    State(state): State<AppState>,
    Extension(user): Extension<Option<User>>,
    Path(session_id): Path<i64>,
    Query(query): Query<DataQuery>,
) -> Response {
    let Some(user) = user else {
        return StatusCode::UNAUTHORIZED.into_response();
    };

    // Verify ownership (admins can access any session).
    let user_id_filter = if user.is_admin { None } else { Some(user.id) };
    match InterferometrySession::fetch_one(
        state.database_connection.clone(),
        session_id,
        user_id_filter,
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

    // Bound per-request size so a page refresh on a long session (or an old
    // client that keeps `after_id=0`) can't pull down every row at once. The
    // client paginates via `after_id`; it will catch up within a few polls.
    let rows = match InterferometryVisibility::fetch_for_session(
        state.database_connection.clone(),
        session_id,
        query.after_id,
        MAX_VISIBILITY_ROWS_PER_REQUEST,
    )
    .await
    {
        Ok(v) => v,
        Err(e) => {
            error!("fetch visibilities: {e:?}");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    let running = state
        .active_correlator
        .lock()
        .await
        .as_ref()
        .is_some_and(|c| c.session_id == session_id);
    Json(SessionDataResponse { rows, running }).into_response()
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
