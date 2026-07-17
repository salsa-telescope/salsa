use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use std::time::Instant;

use askama::Template;
use axum::body::Bytes;
use axum::{
    Extension, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode, header},
    response::{Html, IntoResponse, Response},
    routing::get,
};
use base64::{Engine, prelude::BASE64_STANDARD};
use tokio::sync::Mutex;
use tracing::{debug, error, info};

use crate::app::AppState;
use crate::i18n::Language;
use crate::models::booking::Booking;
use crate::models::guest::GuestSession;
use crate::models::maintenance::fetch_maintenance_set;
use crate::models::telescope_types::{TelescopeStatus, TelescopeTarget};
use crate::models::user::User;
use crate::routes::index::render_main;

/// Age past which we treat the cached webcam image as definitely broken.
const WEBCAM_VERY_STALE_SECS: u64 = 300;
/// Age past which we visibly mark the image as stale (but still show it).
const WEBCAM_STALE_SECS: u64 = 30;

/// Width of the downsampled panorama served to clients. The camera frame is
/// 4K, but the page never shows it larger than roughly this.
const PANORAMA_WIDTH: u32 = 1280;

struct CachedFrames {
    /// Top 32:9 strip of the camera frame, downsampled to PANORAMA_WIDTH.
    panorama: Bytes,
    /// Per-telescope close-ups cut from the full-resolution frame,
    /// keyed by telescope id.
    crops: HashMap<String, Bytes>,
    fetched_at: Instant,
}

#[derive(Clone)]
struct WebcamState {
    snapshot_url: String,
    cache: Arc<Mutex<Option<Arc<CachedFrames>>>>,
    app_state: AppState,
}

static HTTP_CLIENT: OnceLock<reqwest::Client> = OnceLock::new();

fn http_client() -> &'static reqwest::Client {
    HTTP_CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .danger_accept_invalid_certs(true)
            .connect_timeout(std::time::Duration::from_secs(3))
            .timeout(std::time::Duration::from_secs(5))
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
        let app_state_clone = state.app_state.clone();
        tokio::spawn(async move {
            // The camera needs ~1 s to encode a 4K snapshot, so this loop
            // effectively runs at about 1 Hz. The interval is only a floor
            // that keeps us from hammering the camera if it responds faster.
            let mut interval = tokio::time::interval(tokio::time::Duration::from_millis(500));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
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
                let result = match result {
                    Ok(bytes) => {
                        let mut crop_defs = Vec::new();
                        for telescope in app_state_clone.telescopes.get_all().await {
                            if let Ok(info) = telescope.get_info().await
                                && let Some(crop) = info.webcam_crop
                            {
                                crop_defs.push((info.id, crop));
                            }
                        }
                        tokio::task::spawn_blocking(move || process_frame(&bytes, &crop_defs))
                            .await
                            .map_err(|e| format!("frame processing task failed: {e}"))
                            .and_then(|r| r)
                    }
                    Err(msg) => Err(msg),
                };
                match result {
                    Ok(frames) => {
                        if consecutive_failures > 0 {
                            info!(
                                "Webcam snapshot fetch recovered after {consecutive_failures} failed attempts"
                            );
                        }
                        consecutive_failures = 0;
                        *cache_clone.lock().await = Some(Arc::new(frames));
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
        .route("/crop/{telescope}", get(get_webcam_crop))
        .route("/webcam-status", get(get_webcam_status))
        .route("/telescopes", get(get_telescopes_status))
        .with_state(state)
}

/// Decode a camera frame and derive the images served to clients: the
/// downsampled panorama and one full-resolution close-up per telescope.
fn process_frame(jpeg: &[u8], crop_defs: &[(String, [f64; 4])]) -> Result<CachedFrames, String> {
    let full = image::load_from_memory_with_format(jpeg, image::ImageFormat::Jpeg)
        .map_err(|e| format!("image decode error: {e}"))?
        .into_rgb8();
    let (width, height) = full.dimensions();

    // The pages show a 32:9 strip from the top of the (16:9) camera frame.
    let strip_height = (width * 9 / 32).min(height);
    let panorama_height = (PANORAMA_WIDTH * strip_height / width).max(1);
    let strip = image::imageops::crop_imm(&full, 0, 0, width, strip_height).to_image();
    let panorama = image::imageops::resize(
        &strip,
        PANORAMA_WIDTH,
        panorama_height,
        image::imageops::FilterType::Triangle,
    );

    // Crop fractions are relative to the 32:9 viewport: x and w scale with
    // the image width, y and h with the viewport height (width * 9/32).
    let view_height = f64::from(width) * 9.0 / 32.0;
    let mut crops = HashMap::new();
    for (id, c) in crop_defs {
        let x = (c[0] * f64::from(width)) as u32;
        let y = (c[1] * view_height) as u32;
        let w = (c[2] * f64::from(width)) as u32;
        let h = (c[3] * view_height) as u32;
        if x >= width || y >= height || w == 0 || h == 0 {
            continue;
        }
        let region =
            image::imageops::crop_imm(&full, x, y, w.min(width - x), h.min(height - y)).to_image();
        crops.insert(id.clone(), encode_jpeg(&region, 80)?);
    }

    Ok(CachedFrames {
        panorama: encode_jpeg(&panorama, 75)?,
        crops,
        fetched_at: Instant::now(),
    })
}

fn encode_jpeg(img: &image::RgbImage, quality: u8) -> Result<Bytes, String> {
    let mut buf = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, quality)
        .encode_image(img)
        .map_err(|e| format!("image encode error: {e}"))?;
    Ok(Bytes::from(buf))
}

fn jpeg_response(bytes: Bytes) -> Response {
    (
        [
            (header::CONTENT_TYPE, "image/jpeg"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        bytes,
    )
        .into_response()
}

#[derive(Template)]
#[template(path = "live.html")]
struct LiveTemplate {
    initial_snapshot_src: String,
}

async fn get_live_page(
    Extension(lang): Extension<Language>,
    State(state): State<WebcamState>,
    Extension(user): Extension<Option<User>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    // Inline the cached panorama as a data URI so the first image arrives
    // with the page instead of popping in after a second round trip.
    let cached = state.cache.lock().await.clone();
    let initial_snapshot_src = match cached {
        Some(frames) => format!(
            "data:image/jpeg;base64,{}",
            BASE64_STANDARD.encode(&frames.panorama)
        ),
        None => "/live/snapshot".to_string(),
    };
    let content = LiveTemplate {
        initial_snapshot_src,
    }
    .render()
    .expect("Template rendering should always succeed");
    let content = if headers.get("hx-request").is_some() {
        content
    } else {
        render_main(user, lang, content)
    };
    Html(content)
}

async fn get_webcam_snapshot(State(state): State<WebcamState>) -> Response {
    let cached = state.cache.lock().await.clone();
    match cached {
        Some(frames) => jpeg_response(frames.panorama.clone()),
        // 404 (rather than 503) so tower-http's failure classifier doesn't log
        // every poll while the upstream camera is down.
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn get_webcam_crop(
    State(state): State<WebcamState>,
    Path(telescope): Path<String>,
) -> Response {
    let cached = state.cache.lock().await.clone();
    match cached.as_ref().and_then(|f| f.crops.get(&telescope)) {
        Some(bytes) => jpeg_response(bytes.clone()),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

#[derive(Template)]
#[template(path = "webcam_status.html")]
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
    calendar_status: &'static str,
}

#[derive(Template)]
#[template(path = "live_telescopes.html")]
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
    let active_bookings = Booking::fetch_active(state.app_state.database_connection.clone())
        .await
        .unwrap_or_default();
    let active_guests = GuestSession::fetch_all_active(state.app_state.database_connection.clone())
        .await
        .unwrap_or_default();
    let mut telescopes = Vec::new();
    for name in names {
        let Some(telescope) = state.app_state.telescopes.get(&name).await else {
            continue;
        };
        let in_maintenance = maintenance_set.contains(&name);
        let calendar_status = if active_guests.iter().any(|g| g.telescope_id == name) {
            "Guest"
        } else if active_bookings.iter().any(|b| b.telescope_name == name) {
            "Booked"
        } else {
            "Free"
        };
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
            calendar_status,
        });
    }
    Html(
        LiveTelescopesTemplate { telescopes }
            .render()
            .expect("Template rendering should always succeed"),
    )
}
