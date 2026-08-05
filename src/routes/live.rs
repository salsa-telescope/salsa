use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

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
use i18n_embed_fl::fl;
use tokio::sync::Mutex;
use tracing::{debug, error, info};

use crate::app::{AppState, WebcamConfig};
use crate::i18n::Language;
use crate::models::booking::Booking;
use crate::models::guest::GuestSession;
use crate::models::maintenance::fetch_maintenance_set;
use crate::models::telescope_types::{TelescopeStatus, TelescopeTarget};
use crate::models::user::User;
use crate::routes::index::render_main;
use crate::secrets::WebcamCredentials;

/// Age past which we treat the cached webcam image as definitely broken.
const WEBCAM_VERY_STALE_SECS: u64 = 300;
/// Age past which we visibly mark the image as stale (but still show it).
const WEBCAM_STALE_SECS: u64 = 30;

/// Width of the downsampled panorama served to clients. The camera frame is
/// 4K, but the page never shows it larger than roughly this.
const PANORAMA_WIDTH: u32 = 1280;

/// JPEG quality for the per-telescope close-ups. Higher than the panorama's:
/// these are the most zoomed-in thing on the site, cut from a frame the
/// camera has already JPEG-compressed once, so re-encoding them cheaply
/// compounds the loss. At 4K this costs ~13 kB per crop per second.
const CROP_JPEG_QUALITY: u8 = 90;

struct CachedFrames {
    /// The configured region of the camera frame, downsampled to
    /// PANORAMA_WIDTH.
    panorama: Bytes,
    /// Dimensions of `panorama`, so the page can size its box to the picture
    /// it is actually going to receive.
    panorama_size: (u32, u32),
    /// Per-telescope close-ups cut from the full-resolution frame,
    /// keyed by telescope id.
    crops: HashMap<String, Bytes>,
    fetched_at: Instant,
}

#[derive(Clone)]
struct WebcamState {
    /// `None` when no webcam is configured in `.secrets.toml`.
    camera: Option<WebcamCredentials>,
    webcam_config: WebcamConfig,
    cache: Arc<Mutex<Option<Arc<CachedFrames>>>>,
    app_state: AppState,
}

/// Re-login this far before the token's stated lease runs out, so a fetch
/// never races the expiry.
const TOKEN_REFRESH_MARGIN: Duration = Duration::from_secs(300);

/// Render an error with its source chain.
///
/// `reqwest::Error`'s own Display stops at "error sending request for url
/// (...)", which is the same message for a DNS failure, a refused connection
/// and a TLS handshake rejection — the cause only lives in the chain. On a
/// camera we cannot attach a debugger to, that distinction is the whole
/// diagnosis.
fn describe(err: &dyn std::error::Error) -> String {
    let mut out = err.to_string();
    let mut source = err.source();
    while let Some(cause) = source {
        out.push_str(&format!(": {cause}"));
        source = cause.source();
    }
    out
}

/// A camera login token and the instant it stops being usable.
struct Token {
    value: String,
    expires_at: Instant,
}

/// Log in and get a session token.
///
/// The camera used to accept `user=`/`password=` directly on the snapshot
/// URL, which let the whole thing be one static string built at startup. The
/// replacement camera (Reolink RLC-810A, firmware v3.1.0.764) rejects that
/// with `"invalid user"` (rspCode -27), and rejects HTTP Basic and Digest
/// with `"please login first"`, so a token is the only way in.
async fn login(camera: &WebcamCredentials) -> Result<Token, String> {
    let body = serde_json::json!([{
        "cmd": "Login",
        "param": { "User": {
            "Version": "0",
            "userName": camera.username,
            "password": camera.password,
        }}
    }]);
    let resp = http_client()
        .post(format!("{}/cgi-bin/api.cgi?cmd=Login", camera.url))
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("login request error: {}", describe(&e)))?;
    let parsed: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("login response was not JSON: {e}"))?;
    let token = &parsed[0]["value"]["Token"];
    let value = token["name"]
        .as_str()
        .ok_or_else(|| format!("login rejected: {}", parsed[0]["error"]))?;
    // Fall back to a conservative lease if the camera omits one, rather than
    // treating a missing field as "never expires".
    let lease = token["leaseTime"].as_u64().unwrap_or(600);
    Ok(Token {
        value: value.to_string(),
        expires_at: Instant::now()
            + Duration::from_secs(lease).saturating_sub(TOKEN_REFRESH_MARGIN),
    })
}

/// Fetch one full-resolution frame, logging in first if the cached token is
/// missing or about to expire.
///
/// An expired token comes back as a JSON error body with HTTP 200, not as a
/// status code, so the JPEG magic number is what actually tells us the fetch
/// worked. On anything that is not a JPEG the token is dropped, which makes
/// the next cycle log in again.
async fn fetch_frame(
    camera: &WebcamCredentials,
    token: &mut Option<Token>,
) -> Result<Bytes, String> {
    if token
        .as_ref()
        .is_none_or(|t| Instant::now() >= t.expires_at)
    {
        *token = Some(login(camera).await?);
    }
    let Some(current) = token.as_ref() else {
        return Err("no token after login".to_string());
    };
    let url = format!(
        // snapType=main gives the full-resolution frame; the crops served to
        // the observe page need the pixels. (Explicit width/height params are
        // silently ignored by the camera unless they exactly match a stream
        // profile, so we don't use them.)
        "{}/cgi-bin/api.cgi?cmd=Snap&channel=0&rs=salsa&snapType=main&token={}",
        camera.url, current.value
    );
    let result = async {
        let resp = http_client()
            .get(&url)
            .send()
            .await
            .map_err(|e| format!("request error: {}", describe(&e)))?;
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| format!("body read error: {e}"))?;
        if bytes.starts_with(&[0xFF, 0xD8]) {
            Ok(bytes)
        } else {
            Err(format!(
                "camera returned no JPEG: {}",
                String::from_utf8_lossy(&bytes[..bytes.len().min(200)]).replace('\n', " ")
            ))
        }
    }
    .await;
    if result.is_err() {
        *token = None;
    }
    result
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

pub fn routes(
    camera: Option<WebcamCredentials>,
    webcam_config: WebcamConfig,
    app_state: AppState,
) -> Router {
    let state = WebcamState {
        cache: Arc::new(Mutex::new(None)),
        camera,
        webcam_config,
        app_state,
    };

    if let Some(camera) = state.camera.clone() {
        let cache_clone = state.cache.clone();
        let app_state_clone = state.app_state.clone();
        let panorama_crop = state.webcam_config.panorama_crop;
        tokio::spawn(async move {
            // The camera needs ~1 s to encode a 4K snapshot, so this loop
            // effectively runs at about 1 Hz. The interval is only a floor
            // that keeps us from hammering the camera if it responds faster.
            let mut interval = tokio::time::interval(tokio::time::Duration::from_millis(500));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            // Track consecutive failures so we log loudly on transitions
            // (working->broken, broken->working) but stay quiet during a long outage.
            let mut consecutive_failures: u64 = 0;
            let mut token: Option<Token> = None;
            loop {
                interval.tick().await;
                let result = fetch_frame(&camera, &mut token).await;
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
                        tokio::task::spawn_blocking(move || {
                            process_frame(&bytes, &crop_defs, panorama_crop)
                        })
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
fn process_frame(
    jpeg: &[u8],
    crop_defs: &[(String, [f64; 4])],
    panorama_crop: [f64; 4],
) -> Result<CachedFrames, String> {
    let full = image::load_from_memory_with_format(jpeg, image::ImageFormat::Jpeg)
        .map_err(|e| format!("image decode error: {e}"))?
        .into_rgb8();
    let (width, height) = full.dimensions();

    // The live page shows the configured region of the camera frame. It used
    // to be a fixed 32:9 strip off the top, which suited the camera that was
    // there first; the replacement sits lower and wider, so both where the
    // region starts and how much of the width it spans are now configuration.
    // See `WebcamConfig`.
    let strip_x = ((f64::from(width) * panorama_crop[0].clamp(0.0, 1.0)) as u32).min(width - 1);
    let strip_y = ((f64::from(height) * panorama_crop[1].clamp(0.0, 1.0)) as u32).min(height - 1);
    let strip_w =
        ((f64::from(width) * panorama_crop[2].clamp(0.0, 1.0)) as u32).clamp(1, width - strip_x);
    let strip_h =
        ((f64::from(height) * panorama_crop[3].clamp(0.0, 1.0)) as u32).clamp(1, height - strip_y);
    let panorama_height = (PANORAMA_WIDTH * strip_h / strip_w).max(1);
    let strip = image::imageops::crop_imm(&full, strip_x, strip_y, strip_w, strip_h).to_image();
    let panorama = image::imageops::resize(
        &strip,
        PANORAMA_WIDTH,
        panorama_height,
        image::imageops::FilterType::Triangle,
    );

    // Crop fractions: x and w scale with the image width, y and h with the
    // 32:9 viewport height (width * 9/32). The origin stays the top of the
    // camera frame, not the corner of the panorama region, so
    // `panorama_crop` does not shift the close-ups.
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
        crops.insert(id.clone(), encode_jpeg(&region, CROP_JPEG_QUALITY)?);
    }

    Ok(CachedFrames {
        panorama: encode_jpeg(&panorama, 75)?,
        panorama_size: (PANORAMA_WIDTH, panorama_height),
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
    lang: Language,
    initial_snapshot_src: String,
    labels: Vec<TelescopeLabel>,
    /// Width divided by height of the panorama, for the page's box. Taken
    /// from the cached image when there is one so the box always matches the
    /// picture that will land in it.
    panorama_aspect: f64,
}

/// A caption placed under a telescope in the panorama.
///
/// Derived from the same `webcam_crop` that cuts that telescope's close-up,
/// rather than written into the template as a percentage: the two describe
/// the same thing, and when they were maintained separately the captions
/// drifted off their telescopes every time the camera was aimed or replaced.
struct TelescopeLabel {
    name: String,
    /// Centre of the telescope as a percentage of the panorama's width,
    /// mapped through the panorama region — the strip no longer spans the
    /// whole frame, so a crop's frame-relative position is not its position
    /// on screen.
    left_pct: f64,
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
    let initial_snapshot_src = match &cached {
        Some(frames) => format!(
            "data:image/jpeg;base64,{}",
            BASE64_STANDARD.encode(&frames.panorama)
        ),
        None => "/live/snapshot".to_string(),
    };
    let [pano_x, _, pano_w, _] = state.webcam_config.panorama_crop;
    let mut labels = Vec::new();
    for telescope in state.app_state.telescopes.get_all().await {
        if let Ok(info) = telescope.get_info().await
            && let Some(crop) = info.webcam_crop
            && pano_w > 0.0
        {
            // Ids are lowercase in the config; the caption is a display name.
            let mut name = info.id.clone();
            if !name.is_empty() {
                name[..1].make_ascii_uppercase();
            }
            let left_pct = ((crop[0] + crop[2] / 2.0) - pano_x) / pano_w * 100.0;
            // A telescope the panorama does not reach gets no caption, rather
            // than one pinned to an edge it is nowhere near.
            if (0.0..=100.0).contains(&left_pct) {
                labels.push(TelescopeLabel { name, left_pct });
            }
        }
    }

    // Fall back to the configured rectangle's shape before the first frame
    // arrives; the camera frame is 16:9, so its fractions scale by 16/9.
    let panorama_aspect = match &cached {
        Some(frames) if frames.panorama_size.1 > 0 => {
            f64::from(frames.panorama_size.0) / f64::from(frames.panorama_size.1)
        }
        _ => {
            let [_, _, w, h] = state.webcam_config.panorama_crop;
            if h > 0.0 {
                w / h * 16.0 / 9.0
            } else {
                32.0 / 9.0
            }
        }
    };

    let content = LiveTemplate {
        lang,
        initial_snapshot_src,
        labels,
        panorama_aspect,
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

async fn get_webcam_status(
    Extension(lang): Extension<Language>,
    State(state): State<WebcamState>,
) -> Html<String> {
    let template = if state.camera.is_none() {
        WebcamStatusTemplate {
            available: false,
            state_class: "very-stale",
            message: fl!(lang.loader(), "webcam-disabled"),
        }
    } else {
        let cached = state.cache.lock().await.clone();
        match cached {
            Some(c) => {
                let age_secs = c.fetched_at.elapsed().as_secs();
                let age_str = if age_secs < 120 {
                    fl!(lang.loader(), "age-secs", n = age_secs)
                } else {
                    {
                        let mins = age_secs / 60;
                        fl!(lang.loader(), "age-mins", n = mins)
                    }
                };
                let state_class = if age_secs >= WEBCAM_VERY_STALE_SECS {
                    "very-stale"
                } else if age_secs >= WEBCAM_STALE_SECS {
                    "stale"
                } else {
                    "fresh"
                };
                let message = if age_secs >= WEBCAM_VERY_STALE_SECS {
                    fl!(lang.loader(), "webcam-offline", age = age_str)
                } else {
                    fl!(lang.loader(), "webcam-updated", age = age_str)
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
                message: fl!(lang.loader(), "webcam-unavailable"),
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
    lang: Language,
    telescopes: Vec<TelescopeStatusCard>,
}

async fn get_telescopes_status(
    Extension(lang): Extension<Language>,
    State(state): State<WebcamState>,
) -> Html<String> {
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
        LiveTelescopesTemplate { lang, telescopes }
            .render()
            .expect("Template rendering should always succeed"),
    )
}
