use std::sync::OnceLock;

use axum::{
    Extension, Router,
    extract::State,
    http::{HeaderMap, StatusCode, header},
    response::{Html, IntoResponse, Response},
    routing::get,
};

use crate::models::user::User;
use crate::routes::index::render_main;

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
    Router::new()
        .route("/", get(get_webcam_page))
        .route("/snapshot", get(get_webcam_snapshot))
        .with_state(snapshot_url)
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

async fn get_webcam_snapshot(State(snapshot_url): State<String>) -> Response {
    if snapshot_url.is_empty() {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    match http_client().get(&snapshot_url).send().await {
        Ok(resp) => match resp.bytes().await {
            Ok(bytes) => (
                [
                    (header::CONTENT_TYPE, "image/jpeg"),
                    (header::CACHE_CONTROL, "no-store"),
                ],
                bytes,
            )
                .into_response(),
            Err(e) => {
                log::error!("Failed to read webcam snapshot body: {e}");
                StatusCode::BAD_GATEWAY.into_response()
            }
        },
        Err(e) => {
            log::error!("Failed to fetch webcam snapshot: {e}");
            StatusCode::BAD_GATEWAY.into_response()
        }
    }
}
