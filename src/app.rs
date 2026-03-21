use axum::extract::{MatchedPath, State};
use axum::http::{HeaderMap, Request, Uri};
use axum::middleware;
use axum::response::{IntoResponse, Redirect, Response};
use axum::{Router, routing::get};
use rusqlite::Connection;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::Mutex;
use tower_http::services::ServeDir;
use tower_http::trace::TraceLayer;
use tracing::debug_span;

use serde::Deserialize;

use crate::database::create_sqlite_database_on_disk;
use crate::middleware::cookies::cookies_middleware;
use crate::middleware::session::session_middleware;
use crate::models::telescope::{TelescopeCollectionHandle, create_telescope_collection};
use crate::routes;
use crate::secrets::Secrets;
use crate::tle_cache::{TleCacheHandle, start_tle_refresh};

#[derive(Debug, Clone, Deserialize)]
pub struct BookingConfig {
    #[serde(default = "default_max_upcoming_bookings")]
    pub max_upcoming_bookings: u32,
}

fn default_max_upcoming_bookings() -> u32 {
    6
}

impl Default for BookingConfig {
    fn default() -> Self {
        Self {
            max_upcoming_bookings: default_max_upcoming_bookings(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct AdminConfig {
    #[serde(default)]
    pub user_ids: Vec<i64>,
}

#[derive(Deserialize)]
struct SalsaConfig {
    #[serde(default)]
    bookings: BookingConfig,
    #[serde(default)]
    admin: AdminConfig,
}

// Anything that goes in here must be a handle or pointer that can be cloned.
// The underlying state itself should be shared.
#[derive(Clone)]
pub struct AppState {
    pub database_connection: Arc<Mutex<Connection>>,
    pub telescopes: TelescopeCollectionHandle,
    pub secrets: Arc<Secrets>,
    pub booking_config: Arc<BookingConfig>,
    pub admin_config: Arc<AdminConfig>,
    pub tle_cache: TleCacheHandle,
}

pub async fn create_app(config_dir: &Path, database_dir: &Path) -> (Router, AppState) {
    let database_connection = Arc::new(Mutex::new(
        create_sqlite_database_on_disk(database_dir.join("database.sqlite3"))
            .expect("failed to create sqlite database"),
    ));
    let config_path = config_dir.join("config.toml");
    let config_str =
        std::fs::read_to_string(&config_path).expect("config.toml should exist and be readable");
    let salsa_config: SalsaConfig =
        toml::from_str(&config_str).expect("config.toml should be valid toml");
    let booking_config = Arc::new(salsa_config.bookings);
    let admin_config = Arc::new(salsa_config.admin);

    let tle_cache = TleCacheHandle::new();
    start_tle_refresh(tle_cache.clone());
    let telescopes = create_telescope_collection(
        config_path
            .to_str()
            .expect("Config path should be convertible to string"),
        tle_cache.clone(),
    );
    let secrets_path = config_dir.join(".secrets.toml");
    let secrets = Arc::new(
        Secrets::read(
            secrets_path
                .to_str()
                .expect("Secret path should be convertible to string"),
        )
        .expect("Reading .secrets.toml should always succeed"),
    );
    let webcam_snapshot_url = match secrets.webcam.as_ref() {
        Some(creds) => format!(
            "{}/cgi-bin/api.cgi?cmd=Snap&channel=0&rs=salsa&user={}&password={}",
            creds.url, creds.username, creds.password
        ),
        None => String::new(),
    };
    let state = AppState {
        database_connection,
        telescopes,
        secrets,
        booking_config,
        admin_config,
        tle_cache,
    };

    let mut app = Router::new()
        .route("/", get(routes::index::get_index))
        .nest("/account", routes::account::routes(state.clone()))
        .nest("/admin", routes::admin::routes(state.clone()))
        .nest("/support", routes::support::routes())
        .nest("/auth", routes::authentication::routes(state.clone()))
        .nest("/observe", routes::observe::routes(state.clone()))
        .nest("/bookings", routes::booking::routes(state.clone()))
        .nest("/telescope", routes::telescope::routes(state.clone()))
        .nest("/observations", routes::observations::routes(state.clone()))
        .nest("/webcam", routes::webcam::routes(webcam_snapshot_url))
        .layer(
            TraceLayer::new_for_http().make_span_with(|request: &Request<_>| {
                let matched_path = request
                    .extensions()
                    .get::<MatchedPath>()
                    .map(MatchedPath::as_str);
                let requested_path = request.uri().to_string();
                debug_span!(
                    "http_request",
                    method = ?request.method(),
                    matched_path,
                    requested_path,
                )
            }),
        )
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            session_middleware,
        ))
        .route_layer(middleware::from_fn(cookies_middleware));

    let assets_path = "assets";
    log::debug!("serving asserts from {}", assets_path);
    let assets_service = ServeDir::new(assets_path);
    app = app.fallback_service(assets_service);
    (app, state)
}

pub async fn teardown_app(app: AppState) {
    for telescope in app.telescopes.get_all().await {
        telescope.shutdown().await;
    }
}

pub fn create_redirect_app(https_port: u16) -> Router {
    Router::new()
        .fallback(redirect_to_https)
        .with_state(https_port)
}

async fn redirect_to_https(
    uri: Uri,
    State(https_port): State<u16>,
    headers: HeaderMap,
) -> Response {
    let host = headers
        .get(axum::http::header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let hostname = host.split(':').next().unwrap_or(host);
    let https_url = if https_port == 443 {
        format!("https://{hostname}{uri}")
    } else {
        format!("https://{hostname}:{https_port}{uri}")
    };
    Redirect::permanent(&https_url).into_response()
}
