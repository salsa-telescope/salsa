use axum::extract::{MatchedPath, State};
use axum::http::{HeaderMap, Request, Uri};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Redirect, Response};
use axum::{Router, routing::get};
use rusqlite::Connection;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::Mutex;
use tower_http::services::ServeDir;
use tower_http::trace::TraceLayer;
use tracing::{debug, debug_span, warn};

use serde::Deserialize;

use crate::correlator::CorrelatorHandle;
use crate::database::create_sqlite_database_on_disk;
use crate::guest_rate_limiter::GuestStartLimiterHandle;
use crate::login_rate_limiter::LoginRateLimiterHandle;
use crate::middleware::cookies::cookies_middleware;
use crate::middleware::language::language_middleware;
use crate::middleware::session::session_middleware;
use crate::models::session::{purge_expired_pending_oauth2, purge_expired_sessions};
use crate::models::telescope::{TelescopeCollectionHandle, create_telescope_collection};
use crate::routes;
use crate::secrets::Secrets;
use crate::tle_cache::{TleCacheHandle, start_tle_refresh};
use crate::weather_cache::{WeatherCacheHandle, start_weather_refresh};

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
    pub weather_cache: WeatherCacheHandle,
    pub login_rate_limiter: LoginRateLimiterHandle,
    pub guest_start_limiter: GuestStartLimiterHandle,
    /// At most one correlator session running at a time.
    pub active_correlator: Arc<Mutex<Option<CorrelatorHandle>>>,
}

pub async fn create_app(config_dir: &Path, database_dir: &Path) -> (Router, AppState) {
    let database_connection = Arc::new(Mutex::new(
        create_sqlite_database_on_disk(database_dir.join("database.sqlite3"))
            .expect("failed to create sqlite database"),
    ));
    purge_expired_sessions(database_connection.clone())
        .await
        .expect("failed to purge expired sessions on startup");
    purge_expired_pending_oauth2(database_connection.clone())
        .await
        .expect("failed to purge expired pending oauth2 on startup");
    let config_path = config_dir.join("config.toml");
    let config_str = std::fs::read_to_string(&config_path).unwrap_or_default();
    let salsa_config: SalsaConfig =
        toml::from_str(&config_str).expect("config.toml should be valid toml");
    let booking_config = Arc::new(salsa_config.bookings);
    let admin_config = Arc::new(salsa_config.admin);

    let tle_cache = TleCacheHandle::new();
    start_tle_refresh(tle_cache.clone());
    let weather_cache = WeatherCacheHandle::new();
    start_weather_refresh(weather_cache.clone());
    let login_rate_limiter = LoginRateLimiterHandle::new();
    let guest_start_limiter = GuestStartLimiterHandle::new();
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
            // snapType=main gives the full-resolution frame; the crops served to
            // the observe page need the pixels. (Explicit width/height params are
            // silently ignored by the camera unless they exactly match a stream
            // profile, so we don't use them.)
            "{}/cgi-bin/api.cgi?cmd=Snap&channel=0&rs=salsa&user={}&password={}&snapType=main",
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
        weather_cache,
        login_rate_limiter,
        guest_start_limiter,
        active_correlator: Arc::new(Mutex::new(None)),
    };

    let assets_path = "assets";
    debug!("serving asserts from {}", assets_path);
    let app = Router::new()
        .route("/", get(routes::index::get_index))
        .nest(
            "/account",
            routes::account::routes(state.clone()).route_layer(middleware::from_fn(
                crate::middleware::no_guests::reject_guests,
            )),
        )
        .nest("/admin", routes::admin::routes(state.clone()))
        .nest("/about", routes::about::routes())
        .nest("/experiments", routes::experiments::routes())
        .nest("/support", routes::support::routes(state.clone()))
        .nest("/technical", routes::technical::routes())
        .nest("/visibility", routes::visibility::routes())
        .nest("/auth", routes::authentication::routes(state.clone()))
        .nest("/observe", routes::observe::routes(state.clone()))
        .nest(
            "/bookings",
            routes::booking::routes(state.clone()).route_layer(middleware::from_fn(
                crate::middleware::no_guests::reject_guests,
            )),
        )
        .nest("/language", routes::language::routes(state.clone()))
        .nest("/telescope", routes::telescope::routes(state.clone()))
        .nest(
            "/observations",
            routes::observations::routes(state.clone()).route_layer(middleware::from_fn(
                crate::middleware::no_guests::reject_guests,
            )),
        )
        .nest(
            "/live",
            routes::live::routes(webcam_snapshot_url, state.clone()),
        )
        .nest("/weather", routes::weather::routes(state.clone()))
        .nest(
            "/interferometry",
            routes::interferometry::routes(state.clone()),
        )
        // Registered before the layers below so assets get the security
        // headers too (a fallback added after layering would bypass them).
        .fallback_service(ServeDir::new(assets_path))
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
        // Layers run outermost-last: cookies → session → language, so the
        // language resolution sees both the parsed cookies and the user.
        .route_layer(middleware::from_fn(language_middleware))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            session_middleware,
        ))
        .route_layer(middleware::from_fn(cookies_middleware))
        .layer(middleware::from_fn(slow_request_middleware))
        .layer(middleware::from_fn(security_headers_middleware));

    (app, state)
}

pub async fn teardown_app(app: AppState) {
    // Stop any running correlator first — otherwise the session row is left
    // without an end_time and visibility inserts keep firing against a
    // soon-to-be-dropped DB connection.
    let running = app.active_correlator.lock().await.take();
    if let Some(handle) = running {
        crate::routes::interferometry::stop_correlator_session(&app, handle).await;
    }
    for telescope in app.telescopes.get_all().await {
        telescope.shutdown().await;
    }
}

/// Standard security response headers on every response. The CSP allows
/// inline scripts/styles (templates use inline <script> blocks and
/// on*-attributes) but blocks all external origins, so injected content
/// can't load code or exfiltrate to other hosts. `frame-ancestors 'none'`
/// prevents clickjacking; HSTS is ignored by browsers on plain-HTTP dev
/// servers so it's safe to send unconditionally.
async fn security_headers_middleware(req: Request<axum::body::Body>, next: Next) -> Response {
    let mut response = next.run(req).await;
    let headers = response.headers_mut();
    headers.insert(
        "content-security-policy",
        axum::http::HeaderValue::from_static(
            "default-src 'self'; script-src 'self' 'unsafe-inline'; \
             style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; \
             connect-src 'self' wss:; frame-ancestors 'none'; \
             base-uri 'self'; form-action 'self'",
        ),
    );
    headers.insert(
        "strict-transport-security",
        axum::http::HeaderValue::from_static("max-age=63072000"),
    );
    headers.insert(
        "x-content-type-options",
        axum::http::HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        "x-frame-options",
        axum::http::HeaderValue::from_static("DENY"),
    );
    headers.insert(
        "referrer-policy",
        axum::http::HeaderValue::from_static("strict-origin-when-cross-origin"),
    );
    response
}

/// Logs at WARN whenever a request takes longer than this. Helps surface
/// the freezes users have reported: the suspicion is that long blocking
/// FFI work in `measure()` starves the runtime, and the symptom would be
/// otherwise-trivial requests (HTMX polls, asset fetches) ballooning into
/// multi-second waits. Pair with the heartbeat task in main.rs.
async fn slow_request_middleware(req: Request<axum::body::Body>, next: Next) -> Response {
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    let start = std::time::Instant::now();
    let response = next.run(req).await;
    let elapsed = start.elapsed();
    if elapsed > std::time::Duration::from_millis(1000) {
        warn!(
            "slow request: {} {} took {} ms (status {})",
            method,
            path,
            elapsed.as_millis(),
            response.status()
        );
    }
    response
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
