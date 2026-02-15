use crate::app::AppState;
use crate::models::observation::Observation;
use crate::models::user::User;
use crate::routes::index::render_main;
use askama::Template;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Json, Response};
use axum::{Extension, Router, routing::get};
use serde::Serialize;

pub fn routes(state: AppState) -> Router {
    Router::new()
        .route("/", get(get_observations))
        .route("/{observation_id}", get(get_observation_data))
        .with_state(state)
}

#[derive(Template)]
#[template(path = "observations.html")]
struct ObservationsTemplate {
    observations: Vec<Observation>,
}

async fn get_observations(
    Extension(user): Extension<Option<User>>,
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Result<Response, StatusCode> {
    let user = user.ok_or(StatusCode::UNAUTHORIZED)?;
    let content = ObservationsTemplate {
        observations: Observation::fetch_for_user(state.database_connection, &user).await?,
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
}

async fn get_observation_data(
    Extension(user): Extension<Option<User>>,
    Path(observation_id): Path<i64>,
    State(state): State<AppState>,
) -> Result<Response, StatusCode> {
    let user = user.ok_or(StatusCode::UNAUTHORIZED)?;
    let observation =
        Observation::fetch_one(state.database_connection, observation_id, &user)
            .await?
            .ok_or(StatusCode::NOT_FOUND)?;

    let frequencies: Vec<f64> =
        serde_json::from_str(&observation.frequencies_json).map_err(|_| {
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    let amplitudes: Vec<f64> =
        serde_json::from_str(&observation.amplitudes_json).map_err(|_| {
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    Ok(Json(ObservationData {
        frequencies,
        amplitudes,
        telescope_id: observation.telescope_id,
        coordinate_system: observation.coordinate_system,
        target_x: observation.target_x,
        target_y: observation.target_y,
        integration_time_secs: observation.integration_time_secs,
        start_time: observation.start_time.to_rfc3339(),
    })
    .into_response())
}
