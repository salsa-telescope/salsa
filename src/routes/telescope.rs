use crate::app::AppState;
use crate::coords::Direction;
use crate::models::booking::booking_is_active;
use crate::models::telescope::TelescopeHandle;
use crate::models::telescope_types::TelescopeStatus;
use crate::models::telescope_types::{TelescopeError, TelescopeInfo};
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
use tokio::time::Duration;
use tokio_util::bytes::Bytes;

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
    if !booking_is_active(state.database_connection, &user, &telescope_id).await? {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let telescope = state
        .telescopes
        .get(&telescope_id)
        .await
        .ok_or(StatusCode::NOT_FOUND)?;
    // WebSockets come in as a regular HTTP request, that connection is then
    // upgraded to a socket.
    log::debug!("Setting up measurement websocket for {}", telescope_id);
    Ok(upgrade.on_upgrade(move |socket| spectrum_handle_websocket(socket, telescope)))
}

async fn spectrum_handle_websocket(mut socket: WebSocket, telescope: TelescopeHandle) {
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

// HACK: Hacky hack!
impl From<TelescopeError> for TelescopeNotFound {
    fn from(_: TelescopeError) -> Self {
        TelescopeNotFound {}
    }
}

pub async fn get_state(
    State(state): State<AppState>,
    Path(telescope_id): Path<String>,
) -> Result<impl IntoResponse, TelescopeNotFound> {
    let telescope = state
        .telescopes
        .get(&telescope_id)
        .await
        .ok_or(TelescopeNotFound)?;
    Ok(Html(telescope_state(telescope).await?))
}

#[derive(Template)]
#[template(path = "telescope_state.html")]
struct TelescopeStateTemplate {
    info: TelescopeInfo,
    status: String,
    direction: Direction,
}

pub async fn telescope_state(telescope: TelescopeHandle) -> Result<String, TelescopeError> {
    let info = telescope.get_info().await?;
    Ok(TelescopeStateTemplate {
        info: info.clone(),
        status: match &info.status {
            TelescopeStatus::Idle => "Idle".to_string(),
            TelescopeStatus::Slewing => "Slewing".to_string(),
            TelescopeStatus::Tracking => "Tracking".to_string(),
        },
        direction: telescope.get_direction().await?,
    }
    .render()
    .expect("Template rendering should always succeed"))
}
