use crate::app::AppState;
use crate::coords::{
    Direction, Location, horizontal_from_equatorial, horizontal_from_galactic, horizontal_from_sun,
    vlsrcorr_from_galactic,
};
use crate::geoip::lookup_country;
use crate::middleware::session::SESSION_COOKIE_NAME;
use crate::models::booking::{consecutive_booking_end, is_authorized_for_telescope};
use crate::models::guest::{EndReason, GuestSession, StartError, touch_if_guest};
use crate::models::maintenance::fetch_maintenance_set;
use crate::models::observation::Observation;
use crate::models::telescope::Telescope;
use crate::models::telescope_types::{
    ObservationMode, ObservedSpectra, ReceiverConfiguration, ReceiverError, TelescopeError,
    TelescopeInfo, TelescopeStatus, TelescopeTarget,
};
use crate::models::user::User;
use crate::routes::index::render_main;
use crate::routes::telescope::telescope_state;
use crate::tle_cache::TleCacheHandle;

use askama::Template;
use axum::body::Body;
use axum::extract::{ConnectInfo, Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header::SET_COOKIE};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::{Extension, Form};
use axum::{
    Router,
    routing::{get, post},
};
use chrono::{Duration, Utc};
use rusqlite::Connection;
use serde::Deserialize;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{debug, error, info, warn};

pub fn routes(state: AppState) -> Router {
    let observe_routes = Router::new()
        .route("/", get(get_observe))
        .route("/not-available", get(get_observe_not_available))
        .route("/maintenance", get(get_observe_maintenance))
        .route("/preview", get(get_preview))
        .route("/booking-end-time", get(get_booking_end_time))
        .route("/set-target", post(set_target))
        .route("/stop-telescope", post(stop_telescope))
        .route("/observe", post(start_observe))
        .route("/stop", post(stop_observe))
        .route("/satellites", get(get_satellites));
    Router::new()
        .route("/", get(get_observe_landing))
        .route("/guest/start", post(start_guest_session_auto))
        .route("/guest/start/{telescope_id}", post(start_guest_session))
        .route("/guest/end", post(end_guest_session))
        .route("/guest/status", get(get_guest_status))
        .nest("/{telescope_id}", observe_routes)
        .with_state(state)
}

/// Fetch the user's active guest session, if any. Returns `None` for
/// non-guest users (cheap short-circuit) and silently swallows query
/// errors — the banner is best-effort UI.
async fn maybe_guest_session_for(state: &AppState, user: &User) -> Option<GuestSession> {
    if user.provider != "guest" {
        return None;
    }
    GuestSession::fetch_active_for_user(state.database_connection.clone(), user.id)
        .await
        .ok()
        .flatten()
}

#[derive(Template)]
#[template(path = "observe_landing.html")]
struct ObserveLandingTemplate {
    active_bookings: Vec<String>,
    interferometry_available: bool,
}

async fn get_observe_landing(
    State(state): State<AppState>,
    Extension(user): Extension<Option<User>>,
) -> Response {
    let Some(user) = user else {
        return Redirect::to("/auth/login").into_response();
    };
    // Guests don't book — the landing page would be empty for them. Send
    // them straight to their active session's telescope page so the
    // "Observe" nav link is a way back into the session, not a dead end.
    if user.provider == "guest" {
        if let Some(gs) = maybe_guest_session_for(&state, &user).await {
            return Redirect::to(&format!("/observe/{}", gs.telescope_id)).into_response();
        }
        // Cookie still alive but session ended (idle/preempted/etc.) —
        // nothing useful to show; bounce home.
        return Redirect::to("/").into_response();
    }
    let now = chrono::Utc::now();
    let active_bookings =
        crate::models::booking::Booking::fetch_for_user(state.database_connection.clone(), &user)
            .await
            .unwrap_or_default()
            .into_iter()
            .filter(|b| b.active_at(&now))
            .map(|b| b.telescope_name)
            .collect::<Vec<_>>();
    let interferometry_available = if active_bookings.len() >= 2 && user.is_admin {
        let mut all_capable = true;
        for name in &active_bookings {
            match state.telescopes.get(name).await {
                Some(tel) if tel.interferometry_capable().await => {}
                _ => {
                    all_capable = false;
                    break;
                }
            }
        }
        all_capable
    } else {
        false
    };
    let content = ObserveLandingTemplate {
        active_bookings,
        interferometry_available,
    }
    .render()
    .expect("Template should always succeed");
    Html(render_main(Some(user), content)).into_response()
}

#[derive(Deserialize)]
struct PreviewQuery {
    coordinate_system: Option<String>,
    x: Option<String>,
    y: Option<String>,
    #[serde(default)]
    az_offset_deg: f64,
    #[serde(default)]
    el_offset_deg: f64,
}

async fn get_booking_end_time(
    Extension(user): Extension<Option<User>>,
    State(state): State<AppState>,
    Path(telescope_id): Path<String>,
) -> impl IntoResponse {
    let Some(user) = user else {
        return String::new();
    };
    consecutive_booking_end(state.database_connection, &user, &telescope_id)
        .await
        .ok()
        .flatten()
        .map(|t| t.to_rfc3339())
        .unwrap_or_default()
}

async fn get_preview(
    State(state): State<AppState>,
    Path(telescope_id): Path<String>,
    Query(query): Query<PreviewQuery>,
) -> impl IntoResponse {
    let telescope_info = match state.telescopes.get(&telescope_id).await {
        Some(telescope) => telescope.get_info().await.ok(),
        None => None,
    };
    let location = telescope_info
        .as_ref()
        .map(|i| i.location)
        .unwrap_or(Location {
            longitude: 0.0,
            latitude: 0.0,
        });

    let x = query.x.as_deref().and_then(|s| s.parse::<f64>().ok());
    let y = query.y.as_deref().and_then(|s| s.parse::<f64>().ok());

    let az_offset_rad = query.az_offset_deg.to_radians();
    let el_offset_rad = query.el_offset_deg.to_radians();

    let calculated = if query.coordinate_system.as_deref() == Some("stow") {
        telescope_info.and_then(|i| i.stow_position)
    } else if query.coordinate_system.as_deref() == Some("sun") {
        Some(horizontal_from_sun(location, Utc::now()))
    } else if query.coordinate_system.as_deref() == Some("gnss") {
        query
            .x
            .as_deref()
            .and_then(|s| s.parse::<u64>().ok())
            .and_then(|norad_id| {
                state
                    .tle_cache
                    .satellite_direction(norad_id, location, Utc::now())
            })
    } else {
        match (&query.coordinate_system, x, y) {
            (Some(cs), Some(x), Some(y)) => {
                let x_rad = x.to_radians();
                let y_rad = y.to_radians();
                match cs.as_str() {
                    "galactic" => {
                        Some(horizontal_from_galactic(location, Utc::now(), x_rad, y_rad))
                    }
                    "equatorial" => Some(horizontal_from_equatorial(
                        location,
                        Utc::now(),
                        x_rad,
                        y_rad,
                    )),
                    "horizontal" => Some(Direction {
                        azimuth: x_rad,
                        elevation: y_rad,
                    }),
                    _ => None,
                }
            }
            _ => None,
        }
    };

    let calculated = if query.coordinate_system.as_deref() == Some("stow") {
        calculated
    } else {
        calculated.map(|dir| {
            let full_circle = 2.0 * std::f64::consts::PI;
            Direction {
                azimuth: ((dir.azimuth + az_offset_rad) % full_circle + full_circle) % full_circle,
                elevation: dir.elevation + el_offset_rad,
            }
        })
    };

    let current = match state.telescopes.get(&telescope_id).await {
        Some(telescope) => telescope
            .get_info()
            .await
            .ok()
            .and_then(|i| i.current_horizontal),
        None => None,
    };

    let fmt = |d: Option<Direction>, decimals: usize| match d {
        Some(dir) => (
            format!("{:.prec$}", dir.azimuth.to_degrees(), prec = decimals),
            format!("{:.prec$}", dir.elevation.to_degrees(), prec = decimals),
        ),
        None => ("&mdash;".to_string(), "&mdash;".to_string()),
    };

    let (calc_az, calc_el) = fmt(calculated, 3);
    let (cur_az, cur_el) = fmt(current, 1);

    Html(format!(
        r#"<span class="text-gray-400">Calc.</span>
<span class="text-gray-400">Az</span>
<span class="font-mono">{calc_az}&deg;</span>
<span class="text-gray-400">El</span>
<span class="font-mono">{calc_el}&deg;</span>
<span class="text-gray-400">Current</span>
<span class="text-gray-400">Az</span>
<span class="font-mono">{cur_az}&deg;</span>
<span class="text-gray-400">El</span>
<span class="font-mono">{cur_el}&deg;</span>"#
    ))
}

async fn get_satellites(
    State(state): State<AppState>,
    Path(telescope_id): Path<String>,
) -> impl IntoResponse {
    let location = match state.telescopes.get(&telescope_id).await {
        Some(telescope) => telescope
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
    axum::Json(json)
}

#[derive(Deserialize, Debug)]
struct Target {
    x: Option<String>, // Degrees; not required when coordinate_system == "sun" or "stow"
    y: Option<String>, // Degrees; not required when coordinate_system == "sun" or "stow"
    coordinate_system: String,
    #[serde(default)]
    az_offset_deg: f64,
    #[serde(default)]
    el_offset_deg: f64,
}

impl IntoResponse for ReceiverError {
    fn into_response(self) -> Response {
        error_response(format!("{self}"))
    }
}

#[derive(Template)]
#[template(path = "error_callout.html")]
struct ErrorCallout {
    message: String,
}

fn error_response(message: String) -> Response {
    // Create a response that will specifically update the error box on the page.
    let body = ErrorCallout { message }
        .render()
        .expect("Rendering error_callout.html should never fail");
    Response::builder()
        .status(StatusCode::OK) // Needs to be ok to be picked up by htmx.
        .header("HX-Retarget", "#errors")
        .header("HX-Reswap", "innerHTML")
        .body(Body::from(body))
        .expect("Building a response should never fail")
}

async fn set_target(
    State(state): State<AppState>,
    Path(telescope_id): Path<String>,
    Extension(user): Extension<Option<User>>,
    Form(target): Form<Target>,
) -> Result<Response, StatusCode> {
    let user = user.ok_or(StatusCode::UNAUTHORIZED)?;
    if !is_authorized_for_telescope(state.database_connection.clone(), &user, &telescope_id).await?
    {
        return Err(StatusCode::UNAUTHORIZED);
    }
    touch_if_guest(state.database_connection.clone(), &user).await;

    let telescope = state
        .telescopes
        .get(&telescope_id)
        .await
        .ok_or(StatusCode::NOT_FOUND)?;

    let az_offset_rad = target.az_offset_deg.to_radians();
    let el_offset_rad = target.el_offset_deg.to_radians();

    let telescope_target = if target.coordinate_system == "stow" {
        let info = telescope.get_info().await.map_err(|err| {
            error!("Failed to get telescope info: {err}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
        let stow = info.stow_position.ok_or_else(|| {
            error!("No stow position configured for telescope {telescope_id}");
            StatusCode::NOT_FOUND
        })?;
        if let Some(spectra) = telescope.stop_integration().await {
            save_observation(
                state.database_connection,
                &user,
                &info,
                &spectra,
                &state.tle_cache,
            )
            .await;
        }
        TelescopeTarget::Horizontal {
            azimuth: stow.azimuth,
            elevation: stow.elevation,
        }
    } else if target.coordinate_system == "sun" {
        TelescopeTarget::Sun
    } else if target.coordinate_system == "gnss" {
        let Some(norad_id) = target.x.as_deref().and_then(|s| s.parse::<u64>().ok()) else {
            return Ok(error_response("Please select a satellite.".to_string()));
        };
        TelescopeTarget::Satellite { norad_id }
    } else {
        let Some(x_rad) = target
            .x
            .as_deref()
            .and_then(|s| s.parse::<f64>().ok())
            .map(f64::to_radians)
        else {
            return Ok(error_response(
                "Please enter valid coordinates.".to_string(),
            ));
        };
        let Some(y_rad) = target
            .y
            .as_deref()
            .and_then(|s| s.parse::<f64>().ok())
            .map(f64::to_radians)
        else {
            return Ok(error_response(
                "Please enter valid coordinates.".to_string(),
            ));
        };
        match target.coordinate_system.as_str() {
            "galactic" => TelescopeTarget::Galactic {
                longitude: x_rad,
                latitude: y_rad,
            },
            "equatorial" => TelescopeTarget::Equatorial {
                right_ascension: x_rad,
                declination: y_rad,
            },
            "horizontal" => TelescopeTarget::Horizontal {
                azimuth: x_rad,
                elevation: y_rad,
            },
            coordinate_system => {
                debug!("Unkown coordinate system {coordinate_system}");
                return Err(StatusCode::BAD_REQUEST);
            }
        }
    };

    match telescope
        .set_target(telescope_target, az_offset_rad, el_offset_rad)
        .await
    {
        Err(TelescopeError::TargetOutOfElevationRange { min_deg, max_deg }) => {
            return Ok(error_response(format!(
                "Target is out of elevation range ({min_deg:.0}–{max_deg:.0}°)."
            )));
        }
        Err(err) => {
            error!("Failed to set target: {err}.");
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        }
        Ok(_) => {}
    }
    Ok(error_response(String::new()))
}

/// Stop the in-flight integration on `telescope` and persist the resulting
/// spectrum to the database. Three call sites use this: the End button
/// handler, the booking_monitor at handover, and the fixed-duration auto-stop
/// task started by `start_observe`. Each previously inlined the same
/// `get_info → stop_integration → save_observation` sequence; centralising it
/// keeps the behaviour identical and makes future changes (e.g. adding
/// instrumentation) a one-place edit.
pub(crate) async fn stop_and_save_observation(
    telescope: &dyn Telescope,
    connection: Arc<Mutex<Connection>>,
    user: &User,
    tle_cache: &TleCacheHandle,
) {
    // get_info before stop so the snapshot reflects the integration's target.
    let info_result = telescope.get_info().await;
    if let Some(spectra) = telescope.stop_integration().await {
        match info_result {
            Ok(info) => {
                save_observation(connection, user, &info, &spectra, tle_cache).await;
            }
            Err(err) => {
                error!("Failed to get telescope info while stopping integration: {err}");
            }
        }
    }
}

pub(crate) async fn save_observation(
    connection: Arc<Mutex<Connection>>,
    user: &User,
    info: &TelescopeInfo,
    spectra: &ObservedSpectra,
    tle_cache: &TleCacheHandle,
) {
    // Guest sessions are explicitly ephemeral — the live spectrum is shown
    // in the chart while observing, but nothing is persisted to the DB.
    if user.provider == "guest" {
        return;
    }
    let integration_time_secs = spectra.observation_time.as_secs_f64();
    let start_time =
        Utc::now() - Duration::milliseconds(spectra.observation_time.as_millis() as i64);

    let Some(current_target) = info.current_target else {
        return;
    };
    let az_offset_deg = info.az_offset_rad.to_degrees();
    let el_offset_deg = info.el_offset_rad.to_degrees();
    let stored_az_offset = if az_offset_deg.abs() > 1e-9 {
        Some(az_offset_deg)
    } else {
        None
    };
    let stored_el_offset = if el_offset_deg.abs() > 1e-9 {
        Some(el_offset_deg)
    } else {
        None
    };
    let location = info.location;
    let (coordinate_system, target_x, target_y, vlsr_correction_mps): (
        String,
        f64,
        f64,
        Option<f64>,
    ) = match current_target {
        TelescopeTarget::Equatorial {
            right_ascension,
            declination,
        } => (
            "equatorial".into(),
            right_ascension.to_degrees(),
            declination.to_degrees(),
            None,
        ),
        TelescopeTarget::Galactic {
            longitude,
            latitude,
        } => (
            "galactic".into(),
            longitude.to_degrees(),
            latitude.to_degrees(),
            Some(vlsrcorr_from_galactic(longitude, latitude, start_time)),
        ),
        TelescopeTarget::Horizontal { azimuth, elevation } => (
            "horizontal".into(),
            azimuth.to_degrees(),
            elevation.to_degrees(),
            None,
        ),
        TelescopeTarget::Sun => {
            let sun = horizontal_from_sun(location, start_time);
            (
                "sun".into(),
                sun.azimuth.to_degrees(),
                sun.elevation.to_degrees(),
                None,
            )
        }
        TelescopeTarget::Satellite { norad_id } => {
            let (az, el) = tle_cache
                .satellite_direction(norad_id, location, start_time)
                .map(|d| (d.azimuth.to_degrees(), d.elevation.to_degrees()))
                .unwrap_or((0.0, 0.0));
            let name = tle_cache
                .satellite_name(norad_id)
                .unwrap_or_else(|| norad_id.to_string());
            (format!("gnss:{name}"), az, el, None)
        }
    };

    let frequencies_json = match serde_json::to_string(&spectra.frequencies) {
        Ok(json) => json,
        Err(err) => {
            error!("Failed to serialize frequencies: {err}");
            return;
        }
    };
    let amplitudes_json = match serde_json::to_string(&spectra.spectra) {
        Ok(json) => json,
        Err(err) => {
            error!("Failed to serialize amplitudes: {err}");
            return;
        }
    };

    if let Err(err) = Observation::create(
        connection,
        user,
        &info.id,
        start_time,
        &coordinate_system,
        target_x,
        target_y,
        integration_time_secs,
        &frequencies_json,
        &amplitudes_json,
        vlsr_correction_mps,
        stored_az_offset,
        stored_el_offset,
    )
    .await
    {
        error!("Failed to save observation: {err:?}");
    }
}

/// Await a fixed-duration deadline, or never resolve when there is none.
/// Lets the integration monitor's `select!` include an optional timeout arm
/// without special-casing interactive (open-ended) integrations.
async fn wait_for_deadline(deadline: Option<tokio::time::Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}

/// Watch a running integration and stop+save it early on either of two events:
/// the antenna leaving Tracking (a cable-unwrap slew, or the target sinking out
/// of the elevation range — either way `measure()` would keep averaging
/// off-source samples into the block), or an optional fixed-duration deadline.
/// Both race the integration's cancellation token, so a manual End or booking
/// handover pre-empts the monitor and it exits without touching a later run.
#[allow(clippy::too_many_arguments)]
async fn monitor_integration(
    telescope: Arc<dyn Telescope>,
    token: tokio_util::sync::CancellationToken,
    fixed_deadline: Option<tokio::time::Instant>,
    db: Arc<Mutex<Connection>>,
    user: User,
    tle_cache: TleCacheHandle,
    telescope_id: String,
) {
    let mut ticker = tokio::time::interval(std::time::Duration::from_secs(1));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = token.cancelled() => break,
            _ = wait_for_deadline(fixed_deadline) => {
                stop_and_save_observation(telescope.as_ref(), db.clone(), &user, &tle_cache).await;
                break;
            }
            _ = ticker.tick() => {
                match telescope.get_info().await {
                    Ok(info) if info.status != TelescopeStatus::Tracking => {
                        warn!(
                            "Stopping integration on {telescope_id}: antenna left tracking (status {:?}) mid-integration",
                            info.status
                        );
                        stop_and_save_observation(telescope.as_ref(), db.clone(), &user, &tle_cache).await;
                        break;
                    }
                    Ok(_) => {}
                    Err(err) => warn!(
                        "Integration monitor could not read info for {telescope_id}: {err}"
                    ),
                }
            }
        }
    }
}

async fn stop_telescope(
    Extension(user): Extension<Option<User>>,
    State(state): State<AppState>,
    Path(telescope_id): Path<String>,
) -> Result<impl IntoResponse, StatusCode> {
    let user = user.ok_or(StatusCode::UNAUTHORIZED)?;
    if !is_authorized_for_telescope(state.database_connection.clone(), &user, &telescope_id).await?
    {
        return Err(StatusCode::UNAUTHORIZED);
    }
    touch_if_guest(state.database_connection.clone(), &user).await;
    let telescope = state
        .telescopes
        .get(&telescope_id)
        .await
        .ok_or(StatusCode::NOT_FOUND)?;
    let info = telescope.get_info().await.map_err(|err| {
        error!("Failed to get telescope info: {err}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    if let Some(spectra) = telescope.stop_integration().await {
        save_observation(
            state.database_connection,
            &user,
            &info,
            &spectra,
            &state.tle_cache,
        )
        .await;
    }
    telescope.stop().await.map_err(|err| {
        error!("Failed to stop telescope: {err}.");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    Ok(error_response(String::new()))
}

fn default_center_freq_mhz() -> f64 {
    1420.4
}

fn default_ref_freq_mhz() -> f64 {
    1417.9
}

fn default_bandwidth_mhz() -> f64 {
    2.5
}

fn default_gain_db() -> f64 {
    60.0
}

fn default_spectral_channels() -> usize {
    512
}

fn default_rfi_filter() -> bool {
    true
}

#[derive(Deserialize)]
struct ObserveForm {
    #[serde(default)]
    mode: ObservationMode,
    #[serde(default = "default_center_freq_mhz")]
    center_freq_mhz: f64,
    #[serde(default = "default_ref_freq_mhz")]
    ref_freq_mhz: f64,
    #[serde(default = "default_bandwidth_mhz")]
    bandwidth_mhz: f64,
    #[serde(default = "default_gain_db")]
    gain_db: f64,
    #[serde(default = "default_spectral_channels")]
    spectral_channels: usize,
    #[serde(default = "default_rfi_filter")]
    rfi_filter: bool,
    #[serde(default)]
    integration_mode: Option<String>, // "interactive" (default) or "fixed"
    #[serde(default)]
    integration_time_secs: Option<f64>,
}

async fn start_observe(
    Extension(user): Extension<Option<User>>,
    State(state): State<AppState>,
    Path(telescope_id): Path<String>,
    Form(form): Form<ObserveForm>,
) -> Result<impl IntoResponse, StatusCode> {
    let user = user.ok_or(StatusCode::UNAUTHORIZED)?;
    if !is_authorized_for_telescope(state.database_connection.clone(), &user, &telescope_id).await?
    {
        return Err(StatusCode::UNAUTHORIZED);
    }
    touch_if_guest(state.database_connection.clone(), &user).await;

    let telescope = state
        .telescopes
        .get(&telescope_id)
        .await
        .ok_or(StatusCode::NOT_FOUND)?;

    let info = telescope.get_info().await.map_err(|err| {
        error!("Failed to get telescope info: {err}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    if info.status != TelescopeStatus::Tracking {
        return Ok(error_response(
            "Telescope is not tracking. Please wait until it has reached the target.".to_string(),
        ));
    }
    if info.receiver_connected == Some(false) {
        return Ok(error_response(
            "Receiver is not reachable. Check the receiver address and network connection."
                .to_string(),
        ));
    }

    let (freq_min, freq_max) = if user.is_admin {
        (FREQ_MIN_ADMIN_MHZ, FREQ_MAX_ADMIN_MHZ)
    } else {
        (FREQ_MIN_USER_MHZ, FREQ_MAX_USER_MHZ)
    };
    if form.center_freq_mhz < freq_min as f64 || form.center_freq_mhz > freq_max as f64 {
        return Ok(error_response(format!(
            "Center frequency must be between {freq_min} and {freq_max} MHz."
        )));
    }
    if form.ref_freq_mhz < freq_min as f64 || form.ref_freq_mhz > freq_max as f64 {
        return Ok(error_response(format!(
            "Reference frequency must be between {freq_min} and {freq_max} MHz."
        )));
    }
    if form.gain_db < GAIN_MIN_DB || form.gain_db > GAIN_MAX_DB {
        return Ok(error_response(format!(
            "Gain must be between {GAIN_MIN_DB} and {GAIN_MAX_DB} dB."
        )));
    }
    if !VALID_BANDWIDTH_MHZ.contains(&form.bandwidth_mhz) {
        return Err(StatusCode::BAD_REQUEST);
    }
    if !VALID_SPECTRAL_CHANNELS.contains(&form.spectral_channels) {
        return Err(StatusCode::BAD_REQUEST);
    }
    if form.integration_mode.as_deref() == Some("fixed")
        && let Some(secs) = form.integration_time_secs
        && !(secs.is_finite() && secs > 0.0 && secs <= MAX_INTEGRATION_TIME_SECS)
    {
        return Err(StatusCode::BAD_REQUEST);
    }

    telescope
        .set_receiver_configuration(ReceiverConfiguration {
            integrate: true,
            mode: form.mode,
            center_freq_hz: form.center_freq_mhz * 1e6,
            ref_freq_hz: form.ref_freq_mhz * 1e6,
            bandwidth_hz: form.bandwidth_mhz * 1e6,
            gain_db: form.gain_db,
            spectral_channels: form.spectral_channels,
            rfi_filter: form.rfi_filter,
        })
        .await
        .map_err(|err| {
            error!("Failed to set target {err}.");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    // Monitor the running integration and stop+save it early on two events:
    //   * The antenna loses track (e.g. a cable-unwrap slew swings it far off
    //     target). measure() would otherwise keep averaging off-source samples
    //     into the same block, silently corrupting a long integration, so we
    //     cut it off as soon as status leaves Tracking.
    //   * Fixed-duration mode reaches its configured length.
    // Both race against this integration's cancellation token: if the user
    // clicks End first (or the booking ends) the token fires and the task
    // exits without touching whatever integration may follow.
    if let Some(token) = telescope.current_integration_token().await {
        let fixed_deadline = match (form.integration_mode.as_deref(), form.integration_time_secs) {
            (Some("fixed"), Some(secs)) if secs > 0.0 && secs.is_finite() => {
                Some(tokio::time::Instant::now() + std::time::Duration::from_secs_f64(secs))
            }
            _ => None,
        };
        tokio::spawn(monitor_integration(
            telescope.clone(),
            token,
            fixed_deadline,
            state.database_connection.clone(),
            user.clone(),
            state.tle_cache.clone(),
            telescope_id.clone(),
        ));
    }

    let guest_session = maybe_guest_session_for(&state, &user).await;
    let in_maintenance = fetch_maintenance_set(state.database_connection.clone())
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .contains(&telescope_id);
    let content = observe(
        telescope.as_ref(),
        in_maintenance,
        user.is_admin,
        &state.weather_cache,
        guest_session.as_ref(),
    )
    .await?;
    Ok(Html(content).into_response())
}

async fn stop_observe(
    Extension(user): Extension<Option<User>>,
    State(state): State<AppState>,
    Path(telescope_id): Path<String>,
) -> Result<impl IntoResponse, StatusCode> {
    let user = user.ok_or(StatusCode::UNAUTHORIZED)?;
    if !is_authorized_for_telescope(state.database_connection.clone(), &user, &telescope_id).await?
    {
        return Err(StatusCode::UNAUTHORIZED);
    }
    touch_if_guest(state.database_connection.clone(), &user).await;

    let telescope = state
        .telescopes
        .get(&telescope_id)
        .await
        .ok_or(StatusCode::NOT_FOUND)?;

    stop_and_save_observation(
        telescope.as_ref(),
        state.database_connection.clone(),
        &user,
        &state.tle_cache,
    )
    .await;
    let guest_session = maybe_guest_session_for(&state, &user).await;
    let in_maintenance = fetch_maintenance_set(state.database_connection.clone())
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .contains(&telescope_id);
    let content = observe(
        telescope.as_ref(),
        in_maintenance,
        user.is_admin,
        &state.weather_cache,
        guest_session.as_ref(),
    )
    .await?;
    Ok(Html(content))
}

async fn get_observe(
    Extension(user): Extension<Option<User>>,
    State(state): State<AppState>,
    Path(telescope_id): Path<String>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, StatusCode> {
    let user = user.ok_or(StatusCode::UNAUTHORIZED)?;
    if !is_authorized_for_telescope(state.database_connection.clone(), &user, &telescope_id).await?
    {
        return Ok(Redirect::to(&format!("/observe/{telescope_id}/not-available")).into_response());
    }
    let maintenance = fetch_maintenance_set(state.database_connection.clone())
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let in_maintenance = maintenance.contains(&telescope_id);
    if in_maintenance && !user.is_admin {
        return Ok(Redirect::to(&format!("/observe/{telescope_id}/maintenance")).into_response());
    }

    let telescope = state
        .telescopes
        .get(&telescope_id)
        .await
        .ok_or(StatusCode::NOT_FOUND)?;
    let guest_session = maybe_guest_session_for(&state, &user).await;
    let content = observe(
        telescope.as_ref(),
        in_maintenance,
        user.is_admin,
        &state.weather_cache,
        guest_session.as_ref(),
    )
    .await?;
    let content = if headers.get("hx-request").is_some() {
        content
    } else {
        render_main(Some(user), content)
    };
    Ok(Html(content).into_response())
}

#[derive(Template)]
#[template(path = "observe_no_booking.html", escape = "none")]
struct NoBookingTemplate {
    telescope_id: String,
}

async fn get_observe_not_available(
    Extension(user): Extension<Option<User>>,
    Path(telescope_id): Path<String>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, StatusCode> {
    let user = user.ok_or(StatusCode::UNAUTHORIZED)?;
    let content = NoBookingTemplate { telescope_id }
        .render()
        .expect("Template rendering should always succeed");
    let content = if headers.get("hx-request").is_some() {
        content
    } else {
        render_main(Some(user), content)
    };
    Ok(Html(content))
}

#[derive(Template)]
#[template(path = "observe_maintenance.html", escape = "none")]
struct ObserveMaintenanceTemplate {
    telescope_id: String,
}

async fn get_observe_maintenance(
    Extension(user): Extension<Option<User>>,
    Path(telescope_id): Path<String>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, StatusCode> {
    let user = user.ok_or(StatusCode::UNAUTHORIZED)?;
    let content = ObserveMaintenanceTemplate { telescope_id }
        .render()
        .expect("Template rendering should always succeed");
    let content = if headers.get("hx-request").is_some() {
        content
    } else {
        render_main(Some(user), content)
    };
    Ok(Html(content))
}

pub const FREQ_MIN_USER_MHZ: u32 = 1350;
pub const FREQ_MAX_USER_MHZ: u32 = 1600;
pub const FREQ_MIN_ADMIN_MHZ: u32 = 800;
pub const FREQ_MAX_ADMIN_MHZ: u32 = 2300;

// DBSRX2 daughterboard on USRP N210: GC1 (0-73 dB) + BBG (0-15 dB) when
// distributed across all stages via empty-name set_rx_gain.
const GAIN_MIN_DB: f64 = 0.0;
const GAIN_MAX_DB: f64 = 88.0;

// Allowlists mirroring the observe.html <select> options. Values outside
// these sets cannot be produced by the form, so they imply tampering — and
// since `spectral_channels` flows into a `vec![0.0; n]` and `bandwidth_mhz`
// sizes the IQ sample buffer, an unbounded value can OOM-abort the process.
const VALID_BANDWIDTH_MHZ: &[f64] = &[1.0, 2.5, 5.0, 10.0, 25.0];
const VALID_SPECTRAL_CHANNELS: &[usize] = &[64, 128, 256, 512, 1024, 2048, 4096, 8192];
const MAX_INTEGRATION_TIME_SECS: f64 = 3600.0;

#[derive(Template)]
#[template(path = "observe.html", escape = "none")]
struct ObserveTemplate {
    info: TelescopeInfo,
    target_mode: String,
    commanded_x: String,
    commanded_y: String,
    state_html: String,
    in_maintenance: bool,
    is_admin: bool,
    freq_min_mhz: u32,
    freq_max_mhz: u32,
    wind_warning: bool,
    guest_started_at: Option<i64>,
    guest_last_activity_at: Option<i64>,
    guest_idle_secs: i64,
    guest_ceiling_secs: i64,
}

fn fmt_deg(deg: f64) -> String {
    let s = format!("{:.6}", deg);
    let s = s.trim_end_matches('0');
    let s = s.trim_end_matches('.');
    s.to_string()
}

async fn observe(
    telescope: &dyn Telescope,
    in_maintenance: bool,
    is_admin: bool,
    weather_cache: &crate::weather_cache::WeatherCacheHandle,
    guest_session: Option<&GuestSession>,
) -> Result<String, StatusCode> {
    let info = telescope.get_info().await.map_err(|err| {
        error!("Failed to get info {err}");
        StatusCode::NOT_FOUND
    })?;
    let target_mode = match &info.current_target {
        Some(TelescopeTarget::Equatorial { .. }) => "equatorial",
        Some(TelescopeTarget::Galactic { .. }) => "galactic",
        Some(TelescopeTarget::Horizontal { .. }) => "horizontal",
        Some(TelescopeTarget::Sun) => "sun",
        Some(TelescopeTarget::Satellite { .. }) => "gnss",
        None => "galactic",
    }
    .to_string();
    let (commanded_x, commanded_y) = match info.current_target {
        Some(TelescopeTarget::Equatorial {
            right_ascension,
            declination,
        }) => (
            fmt_deg(right_ascension.to_degrees()),
            fmt_deg(declination.to_degrees()),
        ),
        Some(TelescopeTarget::Galactic {
            longitude,
            latitude,
        }) => (
            fmt_deg(longitude.to_degrees()),
            fmt_deg(latitude.to_degrees()),
        ),
        Some(TelescopeTarget::Horizontal { azimuth, elevation }) => (
            fmt_deg(azimuth.to_degrees()),
            fmt_deg(elevation.to_degrees()),
        ),
        Some(TelescopeTarget::Satellite { norad_id }) => (norad_id.to_string(), String::new()),
        Some(TelescopeTarget::Sun) => (String::new(), String::new()),
        // Idle telescope (fresh booking, no continuation): pre-fill a
        // bright HI-line target in the Galactic disk so a first-time
        // visitor can hit Track and see real signal without first
        // having to know what coordinates to enter.
        None => ("140".to_string(), "0".to_string()),
    };
    let state_html = telescope_state(&info.id, telescope).await;
    let (freq_min_mhz, freq_max_mhz) = if is_admin {
        (FREQ_MIN_ADMIN_MHZ, FREQ_MAX_ADMIN_MHZ)
    } else {
        (FREQ_MIN_USER_MHZ, FREQ_MAX_USER_MHZ)
    };
    let wind_warning = info
        .wind_warning_ms
        .zip(weather_cache.get())
        .is_some_and(|(limit, w)| w.wind_avg_ms > limit);
    Ok(ObserveTemplate {
        info,
        target_mode,
        commanded_x,
        commanded_y,
        state_html,
        in_maintenance,
        is_admin,
        freq_min_mhz,
        freq_max_mhz,
        wind_warning,
        guest_started_at: guest_session.map(|g| g.started_at.timestamp()),
        guest_last_activity_at: guest_session.map(|g| g.last_activity_at.timestamp()),
        guest_idle_secs: crate::models::guest::GUEST_IDLE_RELEASE_SECS,
        guest_ceiling_secs: crate::models::guest::GUEST_SESSION_HARD_CEILING_SECS,
    }
    .render()
    .expect("Template rendering should always succeed"))
}

/// Auto-pick variant for the "Observe now" button on the welcome page.
/// Walks the telescope list in the same order the rest of the UI uses
/// (preferred order: torre, vale, brage, then anything else) and tries
/// each one until `GuestSession::start` succeeds. If every telescope is
/// in maintenance or held, returns a small HTML page explaining the
/// situation with a link back home.
async fn start_guest_session_auto(
    Extension(user): Extension<Option<User>>,
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
) -> Response {
    if user.is_some() {
        return Redirect::to("/observe").into_response();
    }
    if state.guest_start_limiter.check_and_record(addr.ip()) {
        return guest_start_error_response("rate_limited");
    }
    let mut names = state.telescopes.get_names().await;
    let preferred_order = ["torre", "vale", "brage"];
    names.sort_by_key(|n| {
        let lower = n.to_lowercase();
        let pos = preferred_order.iter().position(|&p| p == lower.as_str());
        (pos.is_none(), pos.unwrap_or(usize::MAX), lower)
    });
    let maintenance = match fetch_maintenance_set(state.database_connection.clone()).await {
        Ok(m) => m,
        Err(_) => return guest_start_error_response("internal"),
    };

    let country = lookup_country(addr.ip());
    let mut all_in_maintenance = true;
    for name in names {
        if maintenance.contains(&name) {
            continue;
        }
        all_in_maintenance = false;
        match GuestSession::start(state.database_connection.clone(), &name, country.clone()).await {
            Ok((_user, gs, session)) => {
                info!(
                    "Guest session {} started (auto-pick): user_id={} telescope={}",
                    gs.id, gs.user_id, gs.telescope_id
                );
                let cookie = format!(
                    "{SESSION_COOKIE_NAME}={}; SameSite=Lax; HttpOnly; Secure; Path=/; Max-Age=2592000",
                    session.token
                );
                let mut headers = HeaderMap::new();
                headers.insert(
                    SET_COOKIE,
                    cookie.parse().expect("Cookie should be parseable"),
                );
                return (headers, Redirect::to(&format!("/observe/{name}"))).into_response();
            }
            Err(StartError::TelescopeBusy) | Err(StartError::GuestAlreadyActive) => continue,
            Err(StartError::Internal(err)) => {
                error!("Failed to start guest session (auto-pick on {name}): {err:?}");
                return guest_start_error_response("internal");
            }
        }
    }
    if all_in_maintenance {
        guest_start_error_response("all_maintenance")
    } else {
        guest_start_error_response("all_busy")
    }
}

/// Start a guest session for an unauthenticated visitor on the given
/// telescope. Refuses if the telescope is in maintenance, currently held
/// by a real booking, has a real booking starting within the next
/// `GUEST_START_PROTECT_SECS`, or is already held by another guest. On
/// success, sets the session cookie and redirects to the observe page.
async fn start_guest_session(
    Extension(user): Extension<Option<User>>,
    State(state): State<AppState>,
    Path(telescope_id): Path<String>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
) -> Response {
    // Already-logged-in users (real or guest) shouldn't be re-issued a
    // guest cookie. Send them to the regular observe flow.
    if user.is_some() {
        return Redirect::to(&format!("/observe/{telescope_id}")).into_response();
    }
    if state.guest_start_limiter.check_and_record(addr.ip()) {
        return guest_start_error_response("rate_limited");
    }
    // Telescope must exist and not be under maintenance.
    if !state.telescopes.contains_key(&telescope_id).await {
        return guest_start_error_response("not_found");
    }
    let maintenance = match fetch_maintenance_set(state.database_connection.clone()).await {
        Ok(m) => m,
        Err(_) => return guest_start_error_response("internal"),
    };
    if maintenance.contains(&telescope_id) {
        return guest_start_error_response("maintenance");
    }

    let country = lookup_country(addr.ip());
    match GuestSession::start(state.database_connection.clone(), &telescope_id, country).await {
        Ok((user, gs, session)) => {
            info!(
                "Guest session {} started: user_id={} telescope={}",
                gs.id, user.id, gs.telescope_id
            );
            let cookie = format!(
                "{SESSION_COOKIE_NAME}={}; SameSite=Lax; HttpOnly; Secure; Path=/; Max-Age=2592000",
                session.token
            );
            let mut headers = HeaderMap::new();
            headers.insert(
                SET_COOKIE,
                cookie.parse().expect("Cookie should be parseable"),
            );
            (headers, Redirect::to(&format!("/observe/{telescope_id}"))).into_response()
        }
        Err(StartError::TelescopeBusy) => guest_start_error_response("busy"),
        Err(StartError::GuestAlreadyActive) => guest_start_error_response("guest_active"),
        Err(StartError::Internal(err)) => {
            error!("Failed to start guest session: {err:?}");
            guest_start_error_response("internal")
        }
    }
}

/// Bounce a failed guest-start back to the welcome page with a query
/// param the index handler turns into a styled banner. Keeps the user
/// on familiar ground rather than dropping them on a bare error page.
fn guest_start_error_response(reason: &str) -> Response {
    Redirect::to(&format!("/?guest_error={reason}")).into_response()
}

/// Lightweight status probe for the guest banner JS. Returns 200 OK
/// while the caller's guest session is active, 410 Gone otherwise (no
/// guest session, or it has ended). The page polls this every few
/// seconds so a server-side end (idle / ceiling / preempted) — which
/// the rest of the page wouldn't otherwise notice — triggers a clean
/// reload to the welcome page.
async fn get_guest_status(
    Extension(user): Extension<Option<User>>,
    State(state): State<AppState>,
) -> Response {
    let active = match user {
        Some(u) if u.provider == "guest" => {
            GuestSession::fetch_active_for_user(state.database_connection, u.id)
                .await
                .ok()
                .flatten()
                .is_some()
        }
        _ => false,
    };
    if active {
        StatusCode::OK.into_response()
    } else {
        StatusCode::GONE.into_response()
    }
}

/// Explicit "End session" from the guest banner or main-nav button.
/// Stops the telescope cleanly, clears the cached spectrum, and marks
/// the guest_session row ended via the shared `guest_monitor::end_session`
/// helper. Same shutdown order as idle / ceiling / preempted ends, so
/// the next visitor never inherits the previous guest's tracking
/// target or last-seen spectrum.
async fn end_guest_session(
    Extension(user): Extension<Option<User>>,
    State(state): State<AppState>,
) -> Response {
    let Some(user) = user else {
        return Redirect::to("/").into_response();
    };
    if user.provider != "guest" {
        return Redirect::to("/").into_response();
    }
    let active =
        match GuestSession::fetch_active_for_user(state.database_connection.clone(), user.id).await
        {
            Ok(Some(gs)) => gs,
            _ => return Redirect::to("/").into_response(),
        };
    crate::guest_monitor::end_session(&state, &active, EndReason::User).await;
    // The session row is deleted by GuestSession::end, so the cookie is
    // now backed by nothing — proactively clear it on this response too.
    let mut headers = HeaderMap::new();
    headers.insert(
        SET_COOKIE,
        format!("{SESSION_COOKIE_NAME}=deleted; Path=/; expires=Thu, 01 Jan 1970 00:00:00 GMT")
            .parse()
            .expect("Cookie should be parseable"),
    );
    (headers, Redirect::to("/?guest_ended=user")).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coords::{Direction, Location};
    use crate::models::telescope_types::IqBlock;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicBool, Ordering};
    use tokio_util::sync::CancellationToken;

    // A telescope that reports Slewing while an integration is in progress —
    // the exact state the FakeTelescope cannot produce (its own set_target
    // stops integration, and it never autonomously slews), so we mock it to
    // isolate the monitor's tracking-loss stop. Records whether the monitor
    // called stop_integration.
    struct SlewingMock {
        stop_called: Arc<AtomicBool>,
    }

    fn info_slewing_and_measuring() -> TelescopeInfo {
        TelescopeInfo {
            id: "mock".to_string(),
            status: TelescopeStatus::Slewing,
            commanded_horizontal: Some(Direction {
                azimuth: 0.0,
                elevation: 1.0,
            }),
            current_horizontal: Some(Direction {
                azimuth: 3.0,
                elevation: 1.0,
            }),
            current_target: None,
            most_recent_error: None,
            measurement_in_progress: true,
            latest_observation: None,
            stow_position: None,
            az_offset_rad: 0.0,
            el_offset_rad: 0.0,
            location: Location {
                longitude: 0.0,
                latitude: 0.0,
            },
            min_elevation_rad: 0.0,
            max_elevation_rad: std::f64::consts::PI,
            webcam_crop: None,
            receiver_connected: None,
            controller_connected: None,
            wind_warning_ms: None,
            default_ref_freq_mhz: 1417.9,
            default_gain_db: 60.0,
        }
    }

    #[async_trait]
    impl Telescope for SlewingMock {
        async fn get_info(&self) -> Result<TelescopeInfo, TelescopeError> {
            Ok(info_slewing_and_measuring())
        }
        async fn stop_integration(&self) -> Option<ObservedSpectra> {
            self.stop_called.store(true, Ordering::SeqCst);
            Some(ObservedSpectra {
                frequencies: vec![0.0],
                spectra: vec![0.0],
                observation_time: std::time::Duration::from_secs(1),
            })
        }
        async fn set_target(
            &self,
            _t: TelescopeTarget,
            _az: f64,
            _el: f64,
        ) -> Result<TelescopeTarget, TelescopeError> {
            unimplemented!()
        }
        async fn stop(&self) -> Result<(), TelescopeError> {
            unimplemented!()
        }
        async fn set_receiver_configuration(
            &self,
            _c: ReceiverConfiguration,
        ) -> Result<ReceiverConfiguration, ReceiverError> {
            unimplemented!()
        }
        async fn clear_measurements(&self) {
            unimplemented!()
        }
        async fn interferometry_capable(&self) -> bool {
            unimplemented!()
        }
        async fn current_integration_token(&self) -> Option<CancellationToken> {
            unimplemented!()
        }
        async fn shutdown(&self) {
            unimplemented!()
        }
        async fn start_iq_stream(
            &self,
            _c: ReceiverConfiguration,
        ) -> Result<tokio::sync::mpsc::Receiver<IqBlock>, ReceiverError> {
            unimplemented!()
        }
    }

    // The monitor must stop the integration once the telescope reports it is no
    // longer Tracking. A guest user is used so save_observation short-circuits
    // and the in-memory DB is never touched. If the tracking-loss check were
    // removed the monitor would loop forever and the timeout would trip.
    #[tokio::test]
    async fn monitor_stops_integration_when_tracking_lost() {
        let stop_called = Arc::new(AtomicBool::new(false));
        let telescope: Arc<dyn Telescope> = Arc::new(SlewingMock {
            stop_called: stop_called.clone(),
        });
        let db = Arc::new(Mutex::new(
            Connection::open_in_memory().expect("in-memory sqlite"),
        ));
        let guest = User {
            id: 1,
            name: "guest".to_string(),
            provider: "guest".to_string(),
            is_admin: false,
            timezone: None,
        };

        let finished = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            monitor_integration(
                telescope,
                CancellationToken::new(),
                None,
                db,
                guest,
                TleCacheHandle::new(),
                "mock".to_string(),
            ),
        )
        .await;

        assert!(
            finished.is_ok(),
            "monitor should stop the off-target integration and return, not loop"
        );
        assert!(
            stop_called.load(Ordering::SeqCst),
            "monitor should have called stop_integration"
        );
    }
}
