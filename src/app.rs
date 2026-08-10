use axum::extract::{MatchedPath, State};
use axum::http::{HeaderMap, Request, Uri};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Redirect, Response};
use axum::{Router, routing::get};
use rusqlite::Connection;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use tower_http::services::ServeDir;
use tower_http::trace::TraceLayer;
use tracing::{debug, debug_span, info, warn};

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

#[derive(Debug, Clone, Deserialize, Default)]
pub struct GuestConfig {
    /// The order the "Observe now" button hands telescopes out in: the first
    /// one in this list that is free and not in maintenance is the one the
    /// guest gets. Unlike the fixed display order this is a policy call —
    /// which dish gives a newcomer the best first impression — so it belongs
    /// in config, where it can be changed without a rebuild.
    ///
    /// Names not listed here are tried after the listed ones, in display
    /// order; leaving the key out falls back to display order entirely.
    /// Every telescope stays reachable either way, so a dish left off the
    /// list is offered last rather than withheld.
    #[serde(default)]
    pub telescope_priority: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WebcamConfig {
    /// The region of the camera frame shown as the live-page panorama, as
    /// `[x, y, w, h]` fractions of frame width and height. How the site sits
    /// in the frame is a property of where the camera is bolted, so it
    /// belongs here rather than in the binary — re-aiming or replacing the
    /// camera should not need a rebuild and a restart of the live service.
    ///
    /// Unlike `webcam_crop`, all four are plain fractions of the frame. The
    /// page sizes its box from this, so the aspect ratio is whatever the
    /// rectangle is; a very tall one will push the rest of the page down.
    #[serde(default = "default_panorama_crop")]
    pub panorama_crop: [f64; 4],
}

/// Framing measured for the camera installed 2026-08-05: trims the roof
/// overhang out of the top-left corner, and leaves equal margins outside the
/// outermost telescopes. Kept as the default so a deployment that has not set
/// the key still gets a sensible picture.
fn default_panorama_crop() -> [f64; 4] {
    [0.0549, 0.1667, 0.8357, 0.5]
}

impl Default for WebcamConfig {
    fn default() -> Self {
        Self {
            panorama_crop: default_panorama_crop(),
        }
    }
}

#[derive(Deserialize)]
struct SalsaConfig {
    #[serde(default)]
    bookings: BookingConfig,
    #[serde(default)]
    admin: AdminConfig,
    #[serde(default)]
    guests: GuestConfig,
    #[serde(default)]
    webcam: WebcamConfig,
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
    pub guest_config: Arc<GuestConfig>,
    pub tle_cache: TleCacheHandle,
    pub weather_cache: WeatherCacheHandle,
    pub login_rate_limiter: LoginRateLimiterHandle,
    pub guest_start_limiter: GuestStartLimiterHandle,
    /// At most one correlator session running at a time.
    pub active_correlator: Arc<Mutex<Option<CorrelatorHandle>>>,
    /// Cancellation tokens for running repeat series, keyed by telescope name.
    ///
    /// A repeat series outlives the individual integrations it starts, so it
    /// needs a token of its own: each integration's token is replaced by the
    /// next one, and cancelling those would only end the cycle in flight,
    /// leaving the loop to start another. Everything that ends observing —
    /// the Stop button, booking handover, guest session end — cancels this
    /// instead.
    pub active_repeats: Arc<Mutex<HashMap<String, CancellationToken>>>,
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
    let guest_config = Arc::new(salsa_config.guests);
    let webcam_config = salsa_config.webcam;

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

    // A misspelled name in guests.telescope_priority just fails to match and
    // silently does nothing, which looks exactly like the order not taking
    // effect. Name it at startup instead of leaving it to be puzzled over.
    let telescope_names = telescopes.get_names().await;
    for preferred in &guest_config.telescope_priority {
        if !telescope_names
            .iter()
            .any(|name| name.eq_ignore_ascii_case(preferred))
        {
            warn!(
                "guests.telescope_priority lists {preferred:?}, which is not a configured \
                 telescope; it will be ignored"
            );
        }
    }

    let secrets_path = config_dir.join(".secrets.toml");
    let secrets = Arc::new(
        Secrets::read(
            secrets_path
                .to_str()
                .expect("Secret path should be convertible to string"),
        )
        .expect("Reading .secrets.toml should always succeed"),
    );
    let state = AppState {
        database_connection,
        telescopes,
        secrets,
        booking_config,
        admin_config,
        guest_config,
        tle_cache,
        weather_cache,
        login_rate_limiter,
        guest_start_limiter,
        active_correlator: Arc::new(Mutex::new(None)),
        active_repeats: Arc::new(Mutex::new(HashMap::new())),
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
            routes::live::routes(state.secrets.webcam.clone(), webcam_config, state.clone()),
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

/// Per-telescope budget during teardown.
///
/// `SalsaTelescope::shutdown` aborts its background tasks and then closes the
/// rotator connection, and both can block on hardware that has stopped
/// answering — the journal regularly shows controllers dropping out. Without a
/// bound, one unresponsive dish stalls the whole teardown and every telescope
/// queued behind it.
const TELESCOPE_SHUTDOWN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// Stop everything that talks to hardware, in the order that keeps the
/// database consistent.
///
/// Every step is bounded and logged. Until now teardown never ran at all in
/// production — the server's graceful shutdown was unbounded, so systemd
/// SIGKILLed the process first — and once it does run, anything that hangs
/// here hangs the stop. The timings below are what tells you which stage is
/// slow, since none of this is reachable with fake telescopes.
pub async fn teardown_app(app: AppState) {
    // Stop any running correlator first — otherwise the session row is left
    // without an end_time and visibility inserts keep firing against a
    // soon-to-be-dropped DB connection.
    let running = app.active_correlator.lock().await.take();
    if let Some(handle) = running {
        let started = std::time::Instant::now();
        crate::routes::interferometry::stop_correlator_session(&app, handle).await;
        info!(
            "teardown: stopped correlator session in {:?}",
            started.elapsed()
        );
    }

    let mut telescopes = Vec::new();
    for name in app.telescopes.get_names().await {
        if let Some(telescope) = app.telescopes.get(&name).await {
            telescopes.push((name, telescope));
        }
    }
    shutdown_telescopes(telescopes).await;
}

/// Shut each telescope down under its own timeout, so one that never returns
/// costs its own budget rather than everyone else's.
async fn shutdown_telescopes(
    telescopes: Vec<(String, Arc<dyn crate::models::telescope::Telescope>)>,
) {
    for (name, telescope) in telescopes {
        let started = std::time::Instant::now();
        info!("teardown: shutting down telescope {name}");
        match tokio::time::timeout(TELESCOPE_SHUTDOWN_TIMEOUT, telescope.shutdown()).await {
            Ok(()) => info!(
                "teardown: shut down telescope {name} in {:?}",
                started.elapsed()
            ),
            Err(_) => warn!(
                "teardown: telescope {name} did not shut down within \
                 {TELESCOPE_SHUTDOWN_TIMEOUT:?}; abandoning it and continuing"
            ),
        }
    }
}

#[cfg(test)]
mod teardown_tests {
    use super::*;
    use crate::models::mock_telescope::MockTelescope;
    use crate::models::telescope::Telescope;

    /// The failure this guards against is the live one: a telescope whose
    /// shutdown blocks on unresponsive hardware must not strand the
    /// telescopes queued behind it, or hold the process until systemd
    /// SIGKILLs it part-way through teardown.
    #[tokio::test(start_paused = true)]
    async fn a_hanging_telescope_does_not_block_the_others() {
        let stuck = MockTelescope::hanging_on_shutdown();
        let healthy = MockTelescope::returning(Ok(crate::models::mock_telescope::mock_info()));

        let telescopes: Vec<(String, Arc<dyn Telescope>)> = vec![
            ("stuck".to_string(), stuck.clone()),
            ("healthy".to_string(), healthy.clone()),
        ];

        // Completes at all only because each shutdown is bounded; without the
        // timeout this future never resolves and the test hangs.
        shutdown_telescopes(telescopes).await;

        assert!(stuck.shutdown_called(), "the stuck telescope was attempted");
        assert!(
            healthy.shutdown_called(),
            "the telescope after the stuck one must still be shut down"
        );
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

/// The port-80 listener: redirects everything to HTTPS, except ACME
/// challenge files when `acme_webroot` is set.
///
/// Serving the challenge here is what lets certbot renew with `--webroot`
/// while SALSA keeps running. Under `--standalone` certbot needs port 80 to
/// itself, which is why renewal used to require stopping the service — a hard
/// restart every ~60 days that could land mid-observation. A matched route
/// wins over the fallback, so challenge files are served as files and
/// everything else still redirects.
///
/// With no webroot configured the router is exactly what it was before: a
/// bare redirect.
pub fn create_redirect_app(https_port: u16, acme_webroot: Option<PathBuf>) -> Router {
    let mut app = Router::new();
    if let Some(webroot) = acme_webroot {
        let challenge_dir = webroot.join(".well-known/acme-challenge");
        debug!("serving ACME challenges from {}", challenge_dir.display());
        app = app.nest_service("/.well-known/acme-challenge", ServeDir::new(challenge_dir));
    }
    app.fallback(redirect_to_https).with_state(https_port)
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

#[cfg(test)]
mod redirect_app_tests {
    use super::*;
    use axum::body::Body;
    use axum::http::StatusCode;
    use tower::ServiceExt;

    async fn get(app: Router, path: &str) -> Response {
        app.oneshot(
            Request::builder()
                .uri(path)
                .header("host", "salsa.example")
                .body(Body::empty())
                .expect("request should build"),
        )
        .await
        .expect("router should respond")
    }

    fn webroot_with_challenge(token: &str, contents: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        let challenge_dir = dir.path().join(".well-known/acme-challenge");
        std::fs::create_dir_all(&challenge_dir).expect("create challenge dir");
        std::fs::write(challenge_dir.join(token), contents).expect("write challenge");
        dir
    }

    /// The whole point of the webroot: certbot's HTTP-01 challenge must come
    /// back as the file it wrote, not as a redirect to HTTPS. A redirect here
    /// fails validation, and renewal fails with it.
    #[tokio::test]
    async fn acme_challenge_is_served_as_a_file() {
        let webroot = webroot_with_challenge("test-token", "token-contents.key-auth");
        let app = create_redirect_app(443, Some(webroot.path().to_path_buf()));

        let response = get(app, "/.well-known/acme-challenge/test-token").await;

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        assert_eq!(&body[..], b"token-contents.key-auth");
    }

    /// Only the challenge path is exempt — everything else, including other
    /// paths under /.well-known, still goes to HTTPS.
    #[tokio::test]
    async fn everything_else_still_redirects_when_a_webroot_is_configured() {
        let webroot = webroot_with_challenge("test-token", "irrelevant");
        let app = create_redirect_app(443, Some(webroot.path().to_path_buf()));

        for path in ["/", "/observe", "/.well-known/other"] {
            let response = get(app.clone(), path).await;
            assert_eq!(
                response.status(),
                StatusCode::PERMANENT_REDIRECT,
                "{path} should redirect"
            );
        }
    }

    /// A missing token must 404 rather than fall through to the redirect:
    /// certbot reads a 301 as a broken challenge setup.
    #[tokio::test]
    async fn unknown_challenge_token_is_not_redirected() {
        let webroot = webroot_with_challenge("test-token", "irrelevant");
        let app = create_redirect_app(443, Some(webroot.path().to_path_buf()));

        let response = get(app, "/.well-known/acme-challenge/no-such-token").await;

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    /// With no webroot configured the router must behave exactly as it did
    /// before the challenge route existed: everything redirects.
    #[tokio::test]
    async fn without_a_webroot_every_path_redirects() {
        let app = create_redirect_app(443, None);

        for path in ["/", "/.well-known/acme-challenge/test-token"] {
            let response = get(app.clone(), path).await;
            assert_eq!(
                response.status(),
                StatusCode::PERMANENT_REDIRECT,
                "{path} should redirect"
            );
        }
    }

    /// The port is carried into the redirect target for non-443 setups, and
    /// omitted for 443. Guards the `https_port == 443` branch.
    #[tokio::test]
    async fn redirect_target_carries_a_non_default_port() {
        for (port, expected) in [
            (443u16, "https://salsa.example/observe"),
            (8443, "https://salsa.example:8443/observe"),
        ] {
            let response = get(create_redirect_app(port, None), "/observe").await;
            assert_eq!(
                response
                    .headers()
                    .get("location")
                    .and_then(|v| v.to_str().ok()),
                Some(expected)
            );
        }
    }
}
