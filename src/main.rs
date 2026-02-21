use app::teardown_app;
use axum_server::tls_rustls::RustlsConfig;
use clap::Parser;
use std::net::SocketAddr;
use std::net::TcpListener;
use std::path::PathBuf;
use tokio::signal;
use tracing::info;

mod app;
mod coords;
mod database;
mod error;
mod logging;
mod middleware;
mod models;
mod routes;
mod secrets;
mod telescope_controller;
mod telescope_tracker;

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

    let listener = TcpListener::bind(addr).unwrap();
    info!("listening on {}", listener.local_addr().unwrap());
    if let Some(port) = args.port
        && port == 0
    {
        // Tests need to know which port to connect to.
        println!("port:{}", listener.local_addr().unwrap().port());
    }

    let handle = axum_server::Handle::new();
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
        let tls_config = RustlsConfig::from_pem_file(cert_file_path, key_file_path)
            .await
            .unwrap();
        axum_server::from_tcp_rustls(listener, tls_config)
            .handle(handle)
            .serve(app.into_make_service())
            .await
            .unwrap();
    } else {
        axum_server::from_tcp(listener)
            .handle(handle)
            .serve(app.into_make_service())
            .await
            .unwrap();
    }

    teardown_app(state).await;
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

    info!("Shutting down");
    handle.graceful_shutdown(None);
}
