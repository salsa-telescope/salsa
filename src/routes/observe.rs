use crate::app::AppState;
use crate::coords::{
    Direction, Location, horizontal_from_equatorial, horizontal_from_galactic, horizontal_from_sun,
    vlsrcorr_from_galactic,
};
use crate::models::booking::{booking_is_active, consecutive_booking_end};
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
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::{Extension, Form};
use axum::{
    Router,
    routing::{get, post},
};
use chrono::{Duration, Utc};
use rusqlite::Connection;
use serde::Deserialize;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{debug, error};

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
        .nest("/{telescope_id}", observe_routes)
        .with_state(state)
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

fn error_response(message: String) -> Response {
    // Create a response that will specifically update the error box on the page.
    let body = if message.is_empty() {
        String::new()
    } else {
        format!(
            "<div class=\"text-sm font-semibold text-red-700 bg-red-50 border border-red-300 rounded px-3 py-2\">{message}</div>"
        )
    };
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
    if !booking_is_active(state.database_connection.clone(), &user, &telescope_id).await? {
        return Err(StatusCode::UNAUTHORIZED);
    }

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

pub(crate) async fn save_observation(
    connection: Arc<Mutex<Connection>>,
    user: &User,
    info: &TelescopeInfo,
    spectra: &ObservedSpectra,
    tle_cache: &TleCacheHandle,
) {
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

async fn stop_telescope(
    Extension(user): Extension<Option<User>>,
    State(state): State<AppState>,
    Path(telescope_id): Path<String>,
) -> Result<impl IntoResponse, StatusCode> {
    let user = user.ok_or(StatusCode::UNAUTHORIZED)?;
    if !booking_is_active(state.database_connection.clone(), &user, &telescope_id).await? {
        return Err(StatusCode::UNAUTHORIZED);
    }
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
}

async fn start_observe(
    Extension(user): Extension<Option<User>>,
    State(state): State<AppState>,
    Path(telescope_id): Path<String>,
    Form(form): Form<ObserveForm>,
) -> Result<impl IntoResponse, StatusCode> {
    let user = user.ok_or(StatusCode::UNAUTHORIZED)?;
    if !booking_is_active(state.database_connection.clone(), &user, &telescope_id).await? {
        return Err(StatusCode::UNAUTHORIZED);
    }

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
    let in_maintenance = fetch_maintenance_set(state.database_connection)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .contains(&telescope_id);
    let content = observe(
        telescope.as_ref(),
        in_maintenance,
        user.is_admin,
        &state.weather_cache,
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
    if !booking_is_active(state.database_connection.clone(), &user, &telescope_id).await? {
        return Err(StatusCode::UNAUTHORIZED);
    }

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
            state.database_connection.clone(),
            &user,
            &info,
            &spectra,
            &state.tle_cache,
        )
        .await;
    }
    let in_maintenance = fetch_maintenance_set(state.database_connection)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .contains(&telescope_id);
    let content = observe(
        telescope.as_ref(),
        in_maintenance,
        user.is_admin,
        &state.weather_cache,
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
    if !booking_is_active(state.database_connection.clone(), &user, &telescope_id).await? {
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
    let content = observe(
        telescope.as_ref(),
        in_maintenance,
        user.is_admin,
        &state.weather_cache,
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

const FREQ_MIN_USER_MHZ: u32 = 1350;
const FREQ_MAX_USER_MHZ: u32 = 1600;
const FREQ_MIN_ADMIN_MHZ: u32 = 800;
const FREQ_MAX_ADMIN_MHZ: u32 = 2300;

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
        Some(TelescopeTarget::Sun) | None => (String::new(), String::new()),
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
    }
    .render()
    .expect("Template rendering should always succeed"))
}
