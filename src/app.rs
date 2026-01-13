use axum::extract::MatchedPath;
use axum::http::Request;
use axum::middleware;
use axum::{Router, routing::get};
use rusqlite::Connection;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::Mutex;
use tower_http::services::ServeDir;
use tower_http::trace::TraceLayer;
use tracing::debug_span;

use crate::database::create_sqlite_database_on_disk;
use crate::middleware::cookies::cookies_middleware;
use crate::middleware::session::session_middleware;
use crate::models::telescope::{TelescopeCollectionHandle, create_telescope_collection};
use crate::routes;
use crate::secrets::Secrets;

// Anything that goes in here must be a handle or pointer that can be cloned.
// The underlying state itself should be shared.
#[derive(Clone)]
pub struct AppState {
    pub database_connection: Arc<Mutex<Connection>>,
    pub telescopes: TelescopeCollectionHandle,
    pub secrets: Arc<Secrets>,
}

pub async fn create_app(database_dir: &Path) -> Router {
    let database_connection = Arc::new(Mutex::new(
        create_sqlite_database_on_disk(database_dir.join("database.sqlite3"))
            .expect("failed to create sqlite database"),
    ));
    let telescopes = create_telescope_collection("telescopes.toml");
    let secrets = Arc::new(
        Secrets::read(".secrets.toml").expect("Reading .secrets.toml should always succeed"),
    );
    let state = AppState {
        database_connection,
        telescopes,
        secrets,
    };

    let mut app = Router::new()
        .route("/", get(routes::index::get_index))
        .nest("/auth", routes::authentication::routes(state.clone()))
        .nest("/observe", routes::observe::routes(state.clone()))
        .nest("/bookings", routes::booking::routes(state.clone()))
        .nest("/telescope", routes::telescope::routes(state.clone()))
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
    app
}
