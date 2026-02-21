use std::sync::{Arc, OnceLock};

use axum::body::Bytes;
use axum::{
    Extension, Router,
    extract::State,
    http::{HeaderMap, StatusCode, header},
    response::{Html, IntoResponse, Response},
    routing::get,
};
use tokio::sync::Mutex;
use tracing::error;

use crate::models::user::User;
use crate::routes::index::render_main;

#[derive(Clone)]
struct WebcamState {
    snapshot_url: String,
    cache: Arc<Mutex<Option<Bytes>>>,
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

pub fn routes(snapshot_url: String) -> Router {
    let state = WebcamState {
        cache: Arc::new(Mutex::new(None)),
        snapshot_url,
    };

    if !state.snapshot_url.is_empty() {
        let cache_clone = state.cache.clone();
        let url_clone = state.snapshot_url.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(2));
            loop {
                interval.tick().await;
                match http_client().get(&url_clone).send().await {
                    Ok(resp) => match resp.bytes().await {
                        Ok(bytes) => *cache_clone.lock().await = Some(bytes),
                        Err(e) => error!("Failed to read webcam snapshot body: {e}"),
                    },
                    Err(e) => error!("Failed to fetch webcam snapshot: {e}"),
                }
            }
        });
    }

    Router::new()
        .route("/", get(get_webcam_page))
        .route("/snapshot", get(get_webcam_snapshot))
        .with_state(state)
}

async fn get_webcam_page(
    Extension(user): Extension<Option<User>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let content = include_str!("../../assets/webcam.html").to_string();
    let content = if headers.get("hx-request").is_some() {
        content
    } else {
        render_main(user, content)
    };
    Html(content)
}

async fn get_webcam_snapshot(State(state): State<WebcamState>) -> Response {
    if state.snapshot_url.is_empty() {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    let cached: Option<Bytes> = state.cache.lock().await.clone();
    match cached {
        Some(bytes) => (
            [
                (header::CONTENT_TYPE, "image/jpeg"),
                (header::CACHE_CONTROL, "no-store"),
            ],
            bytes,
        )
            .into_response(),
        None => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}
