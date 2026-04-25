use std::sync::{Arc, OnceLock};
use std::time::Instant;

use askama::Template;
use axum::body::Bytes;
use axum::{
    Extension, Router,
    extract::State,
    http::{HeaderMap, StatusCode, header},
    response::{Html, IntoResponse, Response},
    routing::get,
};
use tokio::sync::Mutex;
use tracing::{debug, error, info};

use crate::app::AppState;
use crate::models::maintenance::fetch_maintenance_set;
use crate::models::telescope_types::{TelescopeStatus, TelescopeTarget};
use crate::models::user::User;
use crate::routes::index::render_main;

/// Age past which we treat the cached webcam image as definitely broken.
const WEBCAM_VERY_STALE_SECS: u64 = 300;
/// Age past which we visibly mark the image as stale (but still show it).
const WEBCAM_STALE_SECS: u64 = 30;

#[derive(Clone)]
struct CachedSnapshot {
    bytes: Bytes,
    fetched_at: Instant,
}

#[derive(Clone)]
struct WebcamState {
    snapshot_url: String,
    cache: Arc<Mutex<Option<CachedSnapshot>>>,
    app_state: AppState,
}

static HTTP_CLIENT: OnceLock<reqwest::Client> = OnceLock::new();

fn http_client() -> &'static reqwest::Client {
    HTTP_CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .danger_accept_invalid_certs(true)
            .build()
            .expect("Should be able to build HTTP client")
    })
}

pub fn routes(snapshot_url: String, app_state: AppState) -> Router {
    let state = WebcamState {
        cache: Arc::new(Mutex::new(None)),
        snapshot_url,
        app_state,
    };

    if !state.snapshot_url.is_empty() {
        let cache_clone = state.cache.clone();
        let url_clone = state.snapshot_url.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(2));
            // Track consecutive failures so we log loudly on transitions
            // (working->broken, broken->working) but stay quiet during a long outage.
            let mut consecutive_failures: u64 = 0;
            loop {
                interval.tick().await;
                let result: Result<Bytes, String> = match http_client().get(&url_clone).send().await
                {
                    Ok(resp) => resp
                        .bytes()
                        .await
                        .map_err(|e| format!("body read error: {e}")),
                    Err(e) => Err(format!("request error: {e}")),
                };
                match result {
                    Ok(bytes) => {
                        if consecutive_failures > 0 {
                            info!(
                                "Webcam snapshot fetch recovered after {consecutive_failures} failed attempts"
                            );
                        }
                        consecutive_failures = 0;
                        *cache_clone.lock().await = Some(CachedSnapshot {
                            bytes,
                            fetched_at: Instant::now(),
                        });
                    }
                    Err(msg) => {
                        if consecutive_failures == 0 {
                            error!("Failed to fetch webcam snapshot: {msg}");
                        } else {
                            debug!(
                                "Failed to fetch webcam snapshot (attempt {}): {msg}",
                                consecutive_failures + 1
                            );
                        }
                        consecutive_failures = consecutive_failures.saturating_add(1);
                    }
                }
            }
        });
    }

    Router::new()
        .route("/", get(get_live_page))
        .route("/snapshot", get(get_webcam_snapshot))
        .route("/webcam-status", get(get_webcam_status))
        .route("/telescopes", get(get_telescopes_status))
        .with_state(state)
}

#[derive(Template)]
#[template(path = "live.html", escape = "none")]
struct LiveTemplate {}

async fn get_live_page(
    Extension(user): Extension<Option<User>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let content = LiveTemplate {}
        .render()
        .expect("Template rendering should always succeed");
    let content = if headers.get("hx-request").is_some() {
        content
    } else {
        render_main(user, content)
    };
    Html(content)
}

async fn get_webcam_snapshot(State(state): State<WebcamState>) -> Response {
    if state.snapshot_url.is_empty() {
        return StatusCode::NOT_FOUND.into_response();
    }
    let cached: Option<CachedSnapshot> = state.cache.lock().await.clone();
    match cached {
        Some(c) => (
            [
                (header::CONTENT_TYPE, "image/jpeg"),
                (header::CACHE_CONTROL, "no-store"),
            ],
            c.bytes,
        )
            .into_response(),
        // 404 (rather than 503) so tower-http's failure classifier doesn't log
        // every poll while the upstream camera is down.
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

#[derive(Template)]
#[template(path = "webcam_status.html", escape = "none")]
struct WebcamStatusTemplate {
    available: bool,
    state_class: &'static str,
    message: String,
}

async fn get_webcam_status(State(state): State<WebcamState>) -> Html<String> {
    let template = if state.snapshot_url.is_empty() {
        WebcamStatusTemplate {
            available: false,
            state_class: "very-stale",
            message: "Webcam disabled".to_string(),
        }
    } else {
        let cached = state.cache.lock().await.clone();
        match cached {
            Some(c) => {
                let age_secs = c.fetched_at.elapsed().as_secs();
                let age_str = if age_secs < 120 {
                    format!("{age_secs}s ago")
                } else {
                    format!("{}min ago", age_secs / 60)
                };
                let state_class = if age_secs >= WEBCAM_VERY_STALE_SECS {
                    "very-stale"
                } else if age_secs >= WEBCAM_STALE_SECS {
                    "stale"
                } else {
                    "fresh"
                };
                let message = if age_secs >= WEBCAM_VERY_STALE_SECS {
                    format!("Webcam offline — last image {age_str}")
                } else {
                    format!("Updated {age_str}")
                };
                WebcamStatusTemplate {
                    available: true,
                    state_class,
                    message,
                }
            }
            None => WebcamStatusTemplate {
                available: false,
                state_class: "very-stale",
                message: "Webcam unavailable — no image from camera. \
                    Please contact support if this problem persists."
                    .to_string(),
            },
        }
    };
    Html(
        template
            .render()
            .expect("Template rendering should always succeed"),
    )
}

struct TelescopeStatusCard {
    name: String,
    status: String,
    target: Option<String>,
    position: Option<(String, String)>,
    error: String,
    controller_connected: Option<bool>,
    receiver_connected: Option<bool>,
    in_maintenance: bool,
}

#[derive(Template)]
#[template(path = "live_telescopes.html", escape = "none")]
struct LiveTelescopesTemplate {
    telescopes: Vec<TelescopeStatusCard>,
}

async fn get_telescopes_status(State(state): State<WebcamState>) -> Html<String> {
    let mut names = state.app_state.telescopes.get_names().await;
    let preferred_order = ["torre", "vale", "brage"];
    names.sort_by_key(|n| {
        let lower = n.to_lowercase();
        let pos = preferred_order.iter().position(|&p| p == lower.as_str());
        (pos.is_none(), pos.unwrap_or(usize::MAX), lower)
    });
    let maintenance_set = fetch_maintenance_set(state.app_state.database_connection.clone())
        .await
        .unwrap_or_default();
    let mut telescopes = Vec::new();
    for name in names {
        let Some(telescope) = state.app_state.telescopes.get(&name).await else {
            continue;
        };
        let in_maintenance = maintenance_set.contains(&name);
        let (status, target, position, error, controller_connected, receiver_connected) =
            match telescope.get_info().await {
                Ok(info) => {
                    let status = match info.status {
                        TelescopeStatus::Idle => "Idle",
                        TelescopeStatus::Slewing => "Slewing",
                        TelescopeStatus::Tracking => "Tracking",
                    }
                    .to_string();
                    let target = info.current_target.map(|t| match t {
                        TelescopeTarget::Sun => "Sun".to_string(),
                        TelescopeTarget::Satellite { .. } => "GNSS".to_string(),
                        TelescopeTarget::Galactic { .. } => "Galactic".to_string(),
                        TelescopeTarget::Equatorial { .. } => "Equatorial".to_string(),
                        TelescopeTarget::Horizontal { .. } => "Horizontal".to_string(),
                    });
                    let position = info.current_horizontal.map(|d| {
                        (
                            format!("{:.1}°", d.azimuth.to_degrees()),
                            format!("{:.1}°", d.elevation.to_degrees()),
                        )
                    });
                    let error = match info.most_recent_error {
                        Some(err) => format!("{err}"),
                        None => String::new(),
                    };
                    (
                        status,
                        target,
                        position,
                        error,
                        info.controller_connected,
                        info.receiver_connected,
                    )
                }
                Err(_) => ("Offline".to_string(), None, None, String::new(), None, None),
            };
        telescopes.push(TelescopeStatusCard {
            name,
            status,
            target,
            position,
            error,
            controller_connected,
            receiver_connected,
            in_maintenance,
        });
    }
    Html(
        LiveTelescopesTemplate { telescopes }
            .render()
            .expect("Template rendering should always succeed"),
    )
}
