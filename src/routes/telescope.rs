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
    /// Receiver state, carried through even though the controller is
    /// unreachable: the USRP is a separate device and keeps integrating
    /// whether or not the rotator controller answers. The observe page
    /// derives the Begin/Stop pair from these, so reporting zeros here
    /// would let a second integration be started on top of a running one.
    measuring: bool,
    obs_secs: u64,
}

impl TelescopeOfflineTemplate {
    /// The controller failed but `get_info()` still returned, so the receiver
    /// fields are known and can be reported truthfully.
    fn with_receiver_state(lang: Language, telescope_id: &str, info: &TelescopeInfo) -> Self {
        TelescopeOfflineTemplate {
            lang,
            id: telescope_id.to_string(),
            measuring: info.measurement_in_progress,
            obs_secs: info
                .latest_observation
                .as_ref()
                .map_or(0, |obs| obs.observation_time.as_secs()),
        }
    }

    /// `get_info()` itself failed, so nothing is known about the receiver.
    fn unknown_receiver_state(lang: Language, telescope_id: &str) -> Self {
        TelescopeOfflineTemplate {
            lang,
            id: telescope_id.to_string(),
            measuring: false,
            obs_secs: 0,
        }
    }
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
            TelescopeOfflineTemplate::with_receiver_state(lang, telescope_id, &info)
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
        Err(_) => TelescopeOfflineTemplate::unknown_receiver_state(lang, telescope_id)
            .render()
            .expect("Template rendering should always succeed"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::mock_telescope::{MockTelescope, mock_info, observed_for};

    /// A telescope reporting a controller error alongside a live receiver — a
    /// state the fake telescope cannot reach, since it never raises
    /// `TelescopeIOError`.
    fn info_with(
        most_recent_error: Option<TelescopeError>,
        measurement_in_progress: bool,
        observation_time: Duration,
    ) -> TelescopeInfo {
        TelescopeInfo {
            most_recent_error,
            measurement_in_progress,
            latest_observation: Some(observed_for(observation_time)),
            ..mock_info()
        }
    }

    /// The rotator controller and the receiver are separate devices, so an
    /// unreachable controller must not make the page report the receiver as
    /// idle: the observe page would then leave Begin clickable and let a
    /// second integration start on top of the running one.
    #[tokio::test]
    async fn controller_offline_still_reports_the_receiver_as_measuring() {
        for error in [
            TelescopeError::TelescopeIOError("connection reset".to_string()),
            TelescopeError::TelescopeNotConnected,
        ] {
            let telescope =
                MockTelescope::returning(Ok(info_with(Some(error), true, Duration::from_secs(42))));
            let rendered = telescope_state("vale", telescope.as_ref(), Language::default()).await;

            assert!(
                rendered.contains(r#"data-measuring="true""#),
                "offline partial should report the live receiver as measuring, got: {rendered}"
            );
            assert!(
                rendered.contains(r#"data-obs-secs="42""#),
                "offline partial should carry the real integration time, got: {rendered}"
            );
            // The controller-offline banner is unaffected by the fix.
            assert!(
                rendered.contains(r#"data-status="Offline""#),
                "offline partial should still report the controller as offline, got: {rendered}"
            );
        }
    }

    #[tokio::test]
    async fn controller_offline_reports_an_idle_receiver_as_idle() {
        let telescope = MockTelescope::returning(Ok(info_with(
            Some(TelescopeError::TelescopeNotConnected),
            false,
            Duration::from_secs(0),
        )));
        let rendered = telescope_state("vale", telescope.as_ref(), Language::default()).await;

        assert!(rendered.contains(r#"data-measuring="false""#));
        assert!(rendered.contains(r#"data-obs-secs="0""#));
    }

    /// `get_info()` itself failed, so there is no receiver state to report.
    /// Idle is the safe assumption: it keeps Begin available rather than
    /// stranding the page on a Stop button for an integration we cannot see.
    #[tokio::test]
    async fn unreachable_telescope_reports_an_unknown_receiver_as_idle() {
        let telescope = MockTelescope::returning(Err(TelescopeError::TelescopeNotConnected));
        let rendered = telescope_state("vale", telescope.as_ref(), Language::default()).await;

        assert!(rendered.contains(r#"data-measuring="false""#));
        assert!(rendered.contains(r#"data-obs-secs="0""#));
    }
}
