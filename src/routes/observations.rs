use crate::app::AppState;
use crate::fits::{SpectrumMeta, write_spectrum_fits};
use crate::models::observation::Observation;
use crate::models::user::User;
use crate::routes::index::render_main;
use askama::Template;
use axum::extract::{Path, Query, State};
use axum::http::header;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Json, Response};
use axum::{Extension, Router, routing::get};
use serde::{Deserialize, Serialize};

const PAGE_SIZE: i64 = 10;

pub fn routes(state: AppState) -> Router {
    Router::new()
        .route("/", get(get_observations))
        .route(
            "/{observation_id}",
            get(get_observation_data).delete(delete_observation),
        )
        .route("/{observation_id}/csv", get(get_observation_csv))
        .route("/{observation_id}/fits", get(get_observation_fits))
        .with_state(state)
}

#[derive(Deserialize)]
struct PageQuery {
    page: Option<usize>,
}

#[derive(Template)]
#[template(path = "observations.html")]
struct ObservationsTemplate {
    observations: Vec<Observation>,
    current_page: usize,
    total_pages: usize,
    prev_page: Option<usize>,
    next_page: Option<usize>,
    total_count: i64,
}

fn build_observations_template(
    observations: Vec<Observation>,
    total_count: i64,
    current_page: usize,
) -> ObservationsTemplate {
    let total_pages = ((total_count as usize).saturating_sub(1) / PAGE_SIZE as usize) + 1;
    let current_page = current_page.min(total_pages.max(1));
    let prev_page = if current_page > 1 {
        Some(current_page - 1)
    } else {
        None
    };
    let next_page = if current_page < total_pages {
        Some(current_page + 1)
    } else {
        None
    };
    ObservationsTemplate {
        observations,
        current_page,
        total_pages,
        prev_page,
        next_page,
        total_count,
    }
}

async fn get_observations(
    Extension(user): Extension<Option<User>>,
    headers: HeaderMap,
    Query(query): Query<PageQuery>,
    State(state): State<AppState>,
) -> Result<Response, StatusCode> {
    let user = user.ok_or(StatusCode::UNAUTHORIZED)?;
    let current_page = query.page.unwrap_or(1).max(1);
    let total_count = Observation::count_for_user(state.database_connection.clone(), &user).await?;
    let total_pages = ((total_count as usize).saturating_sub(1) / PAGE_SIZE as usize) + 1;
    let current_page = current_page.min(total_pages.max(1));
    let offset = ((current_page - 1) as i64) * PAGE_SIZE;
    let observations =
        Observation::fetch_for_user_page(state.database_connection, &user, PAGE_SIZE, offset)
            .await?;
    let content = build_observations_template(observations, total_count, current_page)
        .render()
        .expect("Template rendering should always succeed");
    let content = if headers.get("hx-request").is_some() {
        content
    } else {
        render_main(Some(user), content)
    };
    Ok(Html(content).into_response())
}

async fn delete_observation(
    Extension(user): Extension<Option<User>>,
    Path(observation_id): Path<i64>,
    Query(query): Query<PageQuery>,
    State(state): State<AppState>,
) -> Result<Response, StatusCode> {
    let user = user.ok_or(StatusCode::UNAUTHORIZED)?;
    Observation::delete(state.database_connection.clone(), observation_id, &user).await?;
    let current_page = query.page.unwrap_or(1).max(1);
    let total_count = Observation::count_for_user(state.database_connection.clone(), &user).await?;
    let total_pages = ((total_count as usize).saturating_sub(1) / PAGE_SIZE as usize) + 1;
    let current_page = current_page.min(total_pages.max(1));
    let offset = ((current_page - 1) as i64) * PAGE_SIZE;
    let observations =
        Observation::fetch_for_user_page(state.database_connection, &user, PAGE_SIZE, offset)
            .await?;
    let content = build_observations_template(observations, total_count, current_page)
        .render()
        .expect("Template rendering should always succeed");
    Ok(Html(content).into_response())
}

#[derive(Serialize)]
struct ObservationData {
    frequencies: Vec<f64>,
    amplitudes: Vec<f64>,
    telescope_id: String,
    coordinate_system: String,
    target_x: f64,
    target_y: f64,
    integration_time_secs: f64,
    start_time: String,
    vlsr_correction_mps: Option<f64>,
}

async fn get_observation_data(
    Extension(user): Extension<Option<User>>,
    Path(observation_id): Path<i64>,
    State(state): State<AppState>,
) -> Result<Response, StatusCode> {
    let user = user.ok_or(StatusCode::UNAUTHORIZED)?;
    let observation = Observation::fetch_one(state.database_connection, observation_id, &user)
        .await?
        .ok_or(StatusCode::NOT_FOUND)?;

    let frequencies: Vec<f64> = serde_json::from_str(&observation.frequencies_json)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let amplitudes: Vec<f64> = serde_json::from_str(&observation.amplitudes_json)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(ObservationData {
        frequencies,
        amplitudes,
        telescope_id: observation.telescope_id,
        coordinate_system: observation.coordinate_system,
        target_x: observation.target_x,
        target_y: observation.target_y,
        integration_time_secs: observation.integration_time_secs,
        start_time: observation.start_time.to_rfc3339(),
        vlsr_correction_mps: observation.vlsr_correction_mps,
    })
    .into_response())
}

async fn get_observation_csv(
    Extension(user): Extension<Option<User>>,
    Path(observation_id): Path<i64>,
    State(state): State<AppState>,
) -> Result<Response, StatusCode> {
    let user = user.ok_or(StatusCode::UNAUTHORIZED)?;
    let observation = Observation::fetch_one(state.database_connection, observation_id, &user)
        .await?
        .ok_or(StatusCode::NOT_FOUND)?;

    let frequencies: Vec<f64> = serde_json::from_str(&observation.frequencies_json)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let amplitudes: Vec<f64> = serde_json::from_str(&observation.amplitudes_json)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let has_vlsr = observation.vlsr_correction_mps.is_some();
    let vlsr_mps = observation.vlsr_correction_mps.unwrap_or(0.0);
    let c = 299_792_458.0_f64;
    let f_rest = 1_420_405_751.77_f64;

    let tag = observation.start_time.format("%Y%m%dT%H%M%S").to_string();
    let filename = format!("SALSA-{}-{}.csv", observation.telescope_id, tag);

    let mut csv = String::new();
    csv.push_str("# Origin: SALSA\n");
    csv.push_str(&format!("# Telescope: {}\n", observation.telescope_id));
    csv.push_str(&format!(
        "# Date: {}\n",
        observation.start_time.to_rfc3339()
    ));
    csv.push_str(&format!(
        "# Coordinate system: {}\n",
        observation.coordinate_system
    ));
    csv.push_str(&format!(
        "# Target: {:.4}, {:.4} deg\n",
        observation.target_x, observation.target_y
    ));
    csv.push_str(&format!(
        "# Integration time: {:.0} s\n",
        observation.integration_time_secs
    ));
    if has_vlsr {
        csv.push_str(&format!("# VLSR correction: {:.2} m/s\n", vlsr_mps));
        csv.push_str("# Columns: frequency_hz,amplitude,vlsr_mps\n");
        csv.push_str("frequency_hz,amplitude,vlsr_mps\n");
        for (freq, amp) in frequencies.iter().zip(amplitudes.iter()) {
            let vlsr = -(freq - f_rest) * c / f_rest + vlsr_mps;
            csv.push_str(&format!("{},{},{:.4}\n", freq, amp, vlsr));
        }
    } else {
        csv.push_str("# VLSR correction: not available\n");
        csv.push_str("# Columns: frequency_hz,amplitude\n");
        csv.push_str("frequency_hz,amplitude\n");
        for (freq, amp) in frequencies.iter().zip(amplitudes.iter()) {
            csv.push_str(&format!("{},{}\n", freq, amp));
        }
    }

    Ok((
        [
            (header::CONTENT_TYPE, "text/csv; charset=utf-8"),
            (
                header::CONTENT_DISPOSITION,
                &format!("attachment; filename=\"{}\"", filename),
            ),
        ],
        csv,
    )
        .into_response())
}

async fn get_observation_fits(
    Extension(user): Extension<Option<User>>,
    Path(observation_id): Path<i64>,
    State(state): State<AppState>,
) -> Result<Response, StatusCode> {
    let user = user.ok_or(StatusCode::UNAUTHORIZED)?;
    let observation = Observation::fetch_one(state.database_connection, observation_id, &user)
        .await?
        .ok_or(StatusCode::NOT_FOUND)?;

    let frequencies: Vec<f64> = serde_json::from_str(&observation.frequencies_json)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let amplitudes: Vec<f64> = serde_json::from_str(&observation.amplitudes_json)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let tag = observation.start_time.format("%Y%m%dT%H%M%S").to_string();
    let filename = format!("SALSA-{}-{}.fits", observation.telescope_id, tag);

    let fits_bytes = write_spectrum_fits(&SpectrumMeta {
        frequencies: &frequencies,
        amplitudes: &amplitudes,
        telescope_id: &observation.telescope_id,
        coordinate_system: &observation.coordinate_system,
        target_x: observation.target_x,
        target_y: observation.target_y,
        integration_time_secs: observation.integration_time_secs,
        start_time: &observation.start_time.to_rfc3339(),
        vlsr_correction_mps: observation.vlsr_correction_mps,
    });

    Ok((
        [
            (header::CONTENT_TYPE, "application/fits"),
            (
                header::CONTENT_DISPOSITION,
                &format!("attachment; filename=\"{}\"", filename),
            ),
        ],
        fits_bytes,
    )
        .into_response())
}
