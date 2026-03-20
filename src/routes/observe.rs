use crate::app::AppState;
use crate::coords::{
    Direction, Location, horizontal_from_equatorial, horizontal_from_galactic,
    vlsrcorr_from_galactic,
};
use crate::models::booking::{booking_is_active, consecutive_booking_end};
use crate::models::maintenance::fetch_maintenance_set;
use crate::models::observation::Observation;
use crate::models::telescope::Telescope;
use crate::models::telescope_types::{
    ReceiverConfiguration, ReceiverError, TelescopeError, TelescopeInfo, TelescopeStatus,
    TelescopeTarget,
};
use crate::models::user::User;
use crate::routes::index::render_main;
use crate::routes::telescope::telescope_state;

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
use log::{debug, error};
use rusqlite::Connection;
use serde::Deserialize;
use std::sync::Arc;
use tokio::sync::Mutex;

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
        .route("/stop", post(stop_observe));
    Router::new()
        .nest("/{telescope_id}", observe_routes)
        .with_state(state)
}

#[derive(Deserialize)]
struct PreviewQuery {
    coordinate_system: Option<String>,
    x: Option<String>,
    y: Option<String>,
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
    // TODO: get location from telescope definition instead of hardcoding
    let location = Location {
        longitude: 0.20802143022,
        latitude: 1.00170457462,
    };

    let x = query.x.as_deref().and_then(|s| s.parse::<f64>().ok());
    let y = query.y.as_deref().and_then(|s| s.parse::<f64>().ok());

    let calculated = if query.coordinate_system.as_deref() == Some("stow") {
        match state.telescopes.get(&telescope_id).await {
            Some(telescope) => telescope
                .get_info()
                .await
                .ok()
                .and_then(|i| i.stow_position),
            None => None,
        }
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

#[derive(Deserialize, Debug)]
struct Target {
    x: Option<String>, // Degrees; not required when coordinate_system == "stow"
    y: Option<String>, // Degrees; not required when coordinate_system == "stow"
    coordinate_system: String,
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

    let telescope_target = if target.coordinate_system == "stow" {
        let info = telescope.get_info().await.map_err(|err| {
            error!("Failed to get telescope info: {err}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
        let stow = info.stow_position.ok_or_else(|| {
            error!("No stow position configured for telescope {telescope_id}");
            StatusCode::NOT_FOUND
        })?;
        save_latest_observation(state.database_connection, &user, telescope.as_ref()).await;
        telescope
            .set_receiver_configuration(ReceiverConfiguration { integrate: false })
            .await
            .map_err(|err| {
                error!("Failed to stop integration: {err}.");
                StatusCode::INTERNAL_SERVER_ERROR
            })?;
        TelescopeTarget::Horizontal {
            azimuth: stow.azimuth,
            elevation: stow.elevation,
        }
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

    match telescope.set_target(telescope_target).await {
        Err(TelescopeError::TargetBelowHorizon) => {
            return Ok(error_response("Target is below the horizon.".to_string()));
        }
        Err(err) => {
            error!("Failed to set target: {err}.");
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        }
        Ok(_) => {}
    }
    Ok(error_response(String::new()))
}

pub(crate) async fn save_latest_observation(
    connection: Arc<Mutex<Connection>>,
    user: &User,
    telescope: &dyn Telescope,
) {
    let info = match telescope.get_info().await {
        Ok(info) => info,
        Err(err) => {
            error!("Failed to get telescope info for saving observation: {err}");
            return;
        }
    };

    let Some(spectra) = &info.latest_observation else {
        return;
    };

    let integration_time_secs = spectra.observation_time.as_secs_f64();
    let start_time =
        Utc::now() - Duration::milliseconds(spectra.observation_time.as_millis() as i64);

    let Some(current_target) = info.current_target else {
        return;
    };
    let (coordinate_system, target_x, target_y, vlsr_correction_mps) = match current_target {
        TelescopeTarget::Equatorial {
            right_ascension,
            declination,
        } => (
            "equatorial",
            right_ascension.to_degrees(),
            declination.to_degrees(),
            None,
        ),
        TelescopeTarget::Galactic {
            longitude,
            latitude,
        } => (
            "galactic",
            longitude.to_degrees(),
            latitude.to_degrees(),
            Some(vlsrcorr_from_galactic(longitude, latitude, start_time)),
        ),
        TelescopeTarget::Horizontal { azimuth, elevation } => (
            "horizontal",
            azimuth.to_degrees(),
            elevation.to_degrees(),
            None,
        ),
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
        coordinate_system,
        target_x,
        target_y,
        integration_time_secs,
        &frequencies_json,
        &amplitudes_json,
        vlsr_correction_mps,
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
    save_latest_observation(state.database_connection, &user, telescope.as_ref()).await;
    telescope
        .set_receiver_configuration(ReceiverConfiguration { integrate: false })
        .await
        .map_err(|err| {
            error!("Failed to stop integration: {err}.");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    telescope.stop().await.map_err(|err| {
        error!("Failed to stop telescope: {err}.");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    Ok(error_response(String::new()))
}

async fn start_observe(
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
    if info.status != TelescopeStatus::Tracking {
        return Ok(error_response(
            "Telescope is not tracking. Please wait until it has reached the target.".to_string(),
        ));
    }

    telescope
        .set_receiver_configuration(ReceiverConfiguration { integrate: true })
        .await
        .map_err(|err| {
            error!("Failed to set target {err}.");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    let in_maintenance = fetch_maintenance_set(state.database_connection)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .contains(&telescope_id);
    let content = observe(telescope.as_ref(), in_maintenance).await?;
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

    save_latest_observation(state.database_connection.clone(), &user, telescope.as_ref()).await;

    telescope
        .set_receiver_configuration(ReceiverConfiguration { integrate: false })
        .await
        .map_err(|err| {
            error!("Failed to set target {err}.");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    let in_maintenance = fetch_maintenance_set(state.database_connection)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .contains(&telescope_id);
    let content = observe(telescope.as_ref(), in_maintenance).await?;
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
    let content = observe(telescope.as_ref(), in_maintenance).await?;
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

#[derive(Template)]
#[template(path = "observe.html", escape = "none")]
struct ObserveTemplate {
    info: TelescopeInfo,
    target_mode: String,
    commanded_x: String,
    commanded_y: String,
    state_html: String,
    in_maintenance: bool,
}

fn fmt_deg(deg: f64) -> String {
    let s = format!("{:.6}", deg);
    let s = s.trim_end_matches('0');
    let s = s.trim_end_matches('.');
    s.to_string()
}

async fn observe(telescope: &dyn Telescope, in_maintenance: bool) -> Result<String, StatusCode> {
    let info = telescope.get_info().await.map_err(|err| {
        error!("Failed to get info {err}");
        StatusCode::NOT_FOUND
    })?;
    let target_mode = match &info.current_target {
        Some(TelescopeTarget::Equatorial { .. }) => "equatorial",
        Some(TelescopeTarget::Galactic { .. }) => "galactic",
        Some(TelescopeTarget::Horizontal { .. }) => "horizontal",
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
        None => (String::new(), String::new()),
    };
    let state_html = telescope_state(telescope).await.map_err(|err| {
        error!("Failed to get telescope state {err}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    Ok(ObserveTemplate {
        info,
        target_mode,
        commanded_x,
        commanded_y,
        state_html,
        in_maintenance,
    }
    .render()
    .expect("Template rendering should always succeed"))
}
