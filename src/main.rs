use axum_server::tls_rustls::RustlsConfig;
use clap::Parser;
use salsa::{
    app, app::teardown_app, booking_monitor, cert_watcher, guest_monitor, logging, session_monitor,
};
use std::net::SocketAddr;
use std::net::TcpListener;
use std::path::PathBuf;
use tokio::signal;
use tracing::{error, info, warn};

/// How long in-flight connections get to finish once shutdown starts.
///
/// Must stay comfortably under the unit's `TimeoutStopSec` (30 s), so the
/// process exits on its own terms rather than being SIGKILLed part-way
/// through telescope teardown. Ten seconds is far longer than any real
/// request — the slowest logged are ~2 s telescope stops — so in practice
/// only connections that would never close on their own are cut off.
const GRACEFUL_SHUTDOWN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Budget for `teardown_app` once the server has stopped.
///
/// Together with `GRACEFUL_SHUTDOWN_TIMEOUT` this bounds the whole stop at
/// ~20 s, inside the unit's `TimeoutStopSec=30`, so systemd never has to
/// SIGKILL us part-way through closing telescope connections.
const TEARDOWN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    #[arg(short, long, env = "KEY_FILE_PATH")]
    key_file_path: Option<String>,

    #[arg(short, long, env = "CERT_FILE_PATH")]
    cert_file_path: Option<String>,

    #[arg(short, long)]
    port: Option<u16>,

    #[arg(long, default_value = ".")]
    database_dir: PathBuf,

    #[arg(long, default_value = ".")]
    config_dir: PathBuf,

    #[arg(long)]
    log_to_journald: bool,

    /// Directory certbot writes HTTP-01 challenges into, as passed to
    /// `certbot --webroot -w`. When set, the port-80 redirect server serves
    /// `<path>/.well-known/acme-challenge/` as files so renewal can complete
    /// without stopping SALSA to free the port. Only meaningful with TLS on.
    #[arg(long)]
    acme_webroot: Option<PathBuf>,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();
    logging::setup_logging(args.log_to_journald);

    let addr = if let Some(port) = args.port {
        SocketAddr::from(([0, 0, 0, 0], port))
    } else {
        SocketAddr::from(([0, 0, 0, 0], 3000))
    };

    let (app, state) = app::create_app(&args.config_dir, &args.database_dir).await;
    booking_monitor::start(state.clone());
    guest_monitor::start(state.clone());
    session_monitor::start(state.database_connection.clone());

    // Runtime heartbeat: if scheduling is healthy, this loop wakes every
    // ~500 ms. A skew well above that means tokio worker threads are
    // starved (e.g. by long blocking FFI calls) — the symptom users see
    // as a frozen page.
    tokio::spawn(async move {
        let mut last = std::time::Instant::now();
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            let elapsed = last.elapsed();
            if elapsed > std::time::Duration::from_millis(1500) {
                warn!(
                    "tokio runtime stall: heartbeat slept {} ms (expected ~500 ms)",
                    elapsed.as_millis()
                );
            }
            last = std::time::Instant::now();
        }
    });

    let listener = TcpListener::bind(addr).unwrap();
    info!("listening on {}", listener.local_addr().unwrap());
    if let Some(port) = args.port
        && port == 0
    {
        // Tests need to know which port to connect to.
        println!("port:{}", listener.local_addr().unwrap().port());
    }

    let handle = axum_server::Handle::new();
    let shutdown_handle = handle.clone();
    tokio::spawn(handle_shutdown_signal(handle.clone()));

    if let Some(key_file_path) = args.key_file_path {
        // This is needed because rustls tries to magically figure out which provider
        // to use. Our deps require multiple providers so we must pick one.
        rustls::crypto::aws_lc_rs::default_provider()
            .install_default()
            .expect("Should succeed in setting default crypto provider");
        let cert_file_path = args.cert_file_path.unwrap();
        info!(
            "using tls with key file {} and cert file {}",
            key_file_path, cert_file_path
        );
        let tls_config = RustlsConfig::from_pem_file(&cert_file_path, &key_file_path)
            .await
            .unwrap();

        // Picks up certbot renewals in place, so renewal no longer needs to
        // stop the service. Purely additive: if it never fires, the server
        // behaves exactly as it did before.
        cert_watcher::start(
            tls_config.clone(),
            PathBuf::from(&cert_file_path),
            PathBuf::from(&key_file_path),
        );

        let https_port = addr.port();
        let redirect_listener = TcpListener::bind(SocketAddr::from(([0, 0, 0, 0], 80))).unwrap();
        info!("listening for HTTP->HTTPS redirect on port 80");
        let redirect_app = app::create_redirect_app(https_port, args.acme_webroot);
        let redirect_handle = handle.clone();
        tokio::spawn(async move {
            if let Err(e) = axum_server::from_tcp(redirect_listener)
                .handle(redirect_handle)
                .serve(redirect_app.into_make_service())
                .await
            {
                error!("HTTP redirect server error: {e}");
            }
        });

        // Errors are logged rather than unwrapped: a panic here would skip the
        // teardown below, which is the one thing that must happen on the way
        // out.
        if let Err(e) = axum_server::from_tcp_rustls(listener, tls_config)
            .handle(shutdown_handle.clone())
            .serve(app.into_make_service_with_connect_info::<SocketAddr>())
            .await
        {
            error!("HTTPS server error: {e}");
        }
    } else if let Err(e) = axum_server::from_tcp(listener)
        .handle(shutdown_handle.clone())
        .serve(app.into_make_service_with_connect_info::<SocketAddr>())
        .await
    {
        error!("HTTP server error: {e}");
    }

    // Unconditional, so the journal always shows where the boundary between
    // "waiting for connections" and "tearing down hardware" fell. Without it,
    // a slow stop cannot be attributed to either stage.
    let still_open = shutdown_handle.connection_count();
    info!("http server stopped with {still_open} connection(s) open, starting teardown");
    if still_open > 0 {
        warn!("{still_open} connection(s) were still open at shutdown and were cut off");
    }

    // Bounded as a whole as well as per-telescope: teardown talks to hardware,
    // and the point of this path is that the process exits on its own terms.
    // Exceeding this is a bug worth seeing in the journal, not a reason to sit
    // there until systemd sends SIGKILL.
    let started = std::time::Instant::now();
    match tokio::time::timeout(TEARDOWN_TIMEOUT, teardown_app(state)).await {
        Ok(()) => info!("Teardown complete in {:?}, exiting", started.elapsed()),
        Err(_) => error!("Teardown did not finish within {TEARDOWN_TIMEOUT:?}; exiting anyway"),
    }
}

async fn handle_shutdown_signal(handle: axum_server::Handle) {
    let interrupt = async {
        signal::unix::signal(signal::unix::SignalKind::interrupt())
            .expect("Should succeed installing interrupt signal handler.")
            .recv()
            .await
    };

    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("Should succeed installing terminate signal handler")
            .recv()
            .await;
    };
    tokio::select! {
        _ = interrupt => {
            info!("Received interrupt")
        },
        _ = terminate => {
            info!("Received terminate signal")
        },
    }

    // `None` here would mean an *indefinite* grace period. That is what the
    // process did until now, and because something always holds a connection
    // open — the spectrum WebSocket, or a keep-alive from the observe page's
    // 1 Hz polling — the wait never finished. systemd's TimeoutStopSec then
    // expired and SIGKILLed the process on every single restart, which meant
    // `teardown_app` (below the server in `main`) never ran: telescope
    // controllers were never closed cleanly and a running correlator session
    // was never ended.
    //
    // A bounded period lets in-flight requests finish while guaranteeing the
    // server returns, so teardown actually happens. The connection count is
    // logged so the journal shows whether the limit was generous enough or
    // whether connections are being cut off.
    info!(
        "Shutting down: {} open connection(s), allowing up to {:?} to finish",
        handle.connection_count(),
        GRACEFUL_SHUTDOWN_TIMEOUT
    );
    handle.graceful_shutdown(Some(GRACEFUL_SHUTDOWN_TIMEOUT));
}
