use std::sync::Arc;

use crate::app::AppState;
use crate::coords::{PRACTICAL_ELEVATION_LIMIT_DEG, vlsrcorr_from_galactic};
use crate::i18n::Language;
use crate::models::booking::is_authorized_for_telescope;
use crate::models::telescope::Telescope;
use crate::models::telescope_types::TelescopeStatus;
use crate::models::telescope_types::{TelescopeError, TelescopeInfo, TelescopeTarget};
use crate::models::user::User;
use askama::Template;
use axum::Extension;
use axum::extract::ws::Message;
use axum::{
    Router,
    extract::ws::{WebSocket, WebSocketUpgrade},
    extract::{Path, State},
    http::StatusCode,
    response::{Html, IntoResponse, Response},
    routing::{any, get},
};
use chrono::Utc;
use i18n_embed_fl::fl;
use tokio::time::Duration;
use tokio_util::bytes::Bytes;
use tracing::debug;

pub fn routes(state: AppState) -> Router {
    let telescope_routes = Router::new()
        .route("/state", get(get_state))
        .route("/spectrum", any(spectrum_handle_upgrade));
    Router::new()
        .nest("/{telescope_id}", telescope_routes)
        .with_state(state)
}

async fn spectrum_handle_upgrade(
    upgrade: WebSocketUpgrade,
    Path(telescope_id): Path<String>,
    Extension(user): Extension<Option<User>>,
    State(state): State<AppState>,
) -> Result<impl IntoResponse, StatusCode> {
    let user = user.ok_or(StatusCode::UNAUTHORIZED)?;
    if !is_authorized_for_telescope(state.database_connection, &user, &telescope_id).await? {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let telescope = state
        .telescopes
        .get(&telescope_id)
        .await
        .ok_or(StatusCode::NOT_FOUND)?;
    // WebSockets come in as a regular HTTP request, that connection is then
    // upgraded to a socket.
    debug!("Setting up measurement websocket for {}", telescope_id);
    Ok(upgrade.on_upgrade(move |socket| spectrum_handle_websocket(socket, telescope)))
}

async fn spectrum_handle_websocket(mut socket: WebSocket, telescope: Arc<dyn Telescope>) {
    // Send one-time JSON metadata with VLSR correction
    if let Ok(info) = telescope.get_info().await {
        let vlsr_correction_mps = match info.current_target {
            Some(TelescopeTarget::Galactic {
                longitude,
                latitude,
            }) => Some(vlsrcorr_from_galactic(longitude, latitude, Utc::now())),
            _ => None,
        };
        let json = serde_json::json!({ "vlsr_correction_mps": vlsr_correction_mps });
        if socket
            .send(Message::Text(json.to_string().into()))
            .await
            .is_err()
        {
            return;
        }
    }

    loop {
        let info = telescope.get_info().await;
        // Somehow signal the error ...
        if let Ok(info) = info
            && let Some(observation) = info.latest_observation
        {
            // Needed this temporary vector to convince Bytes::from that it
            // could convert. The underlying buffer is maybe just moved?
            //
            // The data is interleaved (freq, spectrum) into one big array
            // and then sent over the socket.
            let byte_vec: Vec<u8> = observation
                .frequencies
                .iter()
                .zip(observation.spectra.iter())
                .flat_map(|(f, v)| {
                    // Pack frequency and amplitude into 16-byte array.
                    // This is one value sent over the socket.
                    let mut res = [0; 16];
                    res[..8].copy_from_slice(&f.to_le_bytes());
                    res[8..].copy_from_slice(&v.to_le_bytes());
                    res
                })
                .collect();
            match socket.send(Message::Binary(Bytes::from(byte_vec))).await {
                Ok(_) => (),
                // No-one is listening anymore.
                Err(_) => return,
            }
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

#[derive(Debug)]
pub struct TelescopeNotFound;

impl IntoResponse for TelescopeNotFound {
    fn into_response(self) -> Response {
        (StatusCode::NOT_FOUND, "Telescope not found".to_string()).into_response()
    }
}

pub async fn get_state(
    Extension(lang): Extension<Language>,
    State(state): State<AppState>,
    Path(telescope_id): Path<String>,
) -> Result<impl IntoResponse, TelescopeNotFound> {
    let telescope = state
        .telescopes
        .get(&telescope_id)
        .await
        .ok_or(TelescopeNotFound)?;
    Ok(Html(
        telescope_state(&telescope_id, telescope.as_ref(), lang).await,
    ))
}

#[derive(Template)]
#[template(path = "telescope_state.html")]
struct TelescopeStateTemplate {
    lang: Language,
    info: TelescopeInfo,
    /// Machine-readable status ("Idle"/"Slewing"/"Tracking"), emitted as a
    /// data attribute for the observe-page JS; the visible text is
    /// translated separately.
    status: String,
    error: String,
    /// Machine-readable error category for the observe-page JS ("" when
    /// there is no error).
    error_kind: &'static str,
    low_elevation_deg: Option<f64>,
}

#[derive(Template)]
#[template(path = "telescope_state_offline.html")]
struct TelescopeOfflineTemplate {
    lang: Language,
    id: String,
}

pub async fn telescope_state(
    telescope_id: &str,
    telescope: &dyn Telescope,
    lang: Language,
) -> String {
    match telescope.get_info().await {
        Ok(info)
            if matches!(
                info.most_recent_error,
                Some(TelescopeError::TelescopeIOError(_) | TelescopeError::TelescopeNotConnected)
            ) =>
        {
            TelescopeOfflineTemplate {
                lang,
                id: telescope_id.to_string(),
            }
            .render()
            .expect("Template rendering should always succeed")
        }
        Ok(info) => TelescopeStateTemplate {
            lang,
            info: info.clone(),
            // Elevation-range checks only reject targets below the
            // telescope's hard minimum; a commanded position can still sit
            // low enough that the ground degrades the spectrum. Only warn
            // while the telescope is actually going to / on a target; an
            // idle telescope's commanded position is meaningless.
            low_elevation_deg: match &info.status {
                TelescopeStatus::Idle => None,
                TelescopeStatus::Slewing | TelescopeStatus::Tracking => info
                    .commanded_horizontal
                    .map(|dir| dir.elevation.to_degrees())
                    .filter(|el| *el < PRACTICAL_ELEVATION_LIMIT_DEG),
            },
            status: match &info.status {
                TelescopeStatus::Idle => "Idle".to_string(),
                TelescopeStatus::Slewing => "Slewing".to_string(),
                TelescopeStatus::Tracking => "Tracking".to_string(),
            },
            error: match &info.most_recent_error {
                Some(err) => match err {
                    TelescopeError::TargetOutOfElevationRange { min_deg, max_deg } => {
                        fl!(
                            lang.loader(),
                            "state-error-elevation-range",
                            min = format!("{min_deg:.0}"),
                            max = format!("{max_deg:.0}")
                        )
                    }
                    TelescopeError::TelescopeIOError(_) => fl!(lang.loader(), "state-error-io"),
                    TelescopeError::TelescopeNotConnected => {
                        fl!(lang.loader(), "state-error-not-connected")
                    }
                    TelescopeError::ReceiverFailed(msg) => {
                        fl!(lang.loader(), "state-error-receiver", msg = msg.as_str())
                    }
                    // Calibration rejections are reported synchronously to the
                    // admin page and never stored in most_recent_error.
                    TelescopeError::TelescopeBusy => err.to_string(),
                },
                None => "".to_string(),
            },
            error_kind: match &info.most_recent_error {
                Some(TelescopeError::TargetOutOfElevationRange { .. }) => "elevation",
                Some(TelescopeError::TelescopeIOError(_)) => "io",
                Some(TelescopeError::TelescopeNotConnected) => "not-connected",
                Some(TelescopeError::ReceiverFailed(_)) => "receiver",
                Some(TelescopeError::TelescopeBusy) => "busy",
                None => "",
            },
        }
        .render()
        .expect("Template rendering should always succeed"),
        Err(_) => TelescopeOfflineTemplate {
            lang,
            id: telescope_id.to_string(),
        }
        .render()
        .expect("Template rendering should always succeed"),
    }
}
