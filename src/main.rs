use axum_server::tls_rustls::RustlsConfig;
use clap::Parser;
use std::net::SocketAddr;

mod app;
mod coords;
mod database;
mod error;
mod models;
mod routes;
mod telescope_controller;
mod telescope_tracker;
mod template;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    #[arg(short, long, env = "KEY_FILE_PATH")]
    key_file_path: Option<String>,

    #[arg(short, long, env = "CERT_FILE_PATH")]
    cert_file_path: Option<String>,
}

#[tokio::main]
async fn main() {
    env_logger::init();

    let args = Args::parse();

    let addr = SocketAddr::from(([0, 0, 0, 0], 3000));

    let app = app::create_app().await;

    log::info!("listening on {}", addr);
    if let Some(key_file_path) = args.key_file_path {
        let cert_file_path = args.cert_file_path.unwrap();
        log::info!(
            "using tls with key file {} and cert file {}",
            key_file_path,
            cert_file_path
        );
        let tls = RustlsConfig::from_pem_file(cert_file_path, key_file_path)
            .await
            .unwrap();
        axum_server::bind_rustls(addr, tls)
            .serve(app.into_make_service())
            .await
            .unwrap();
    } else {
        axum_server::bind(addr)
            .serve(app.into_make_service())
            .await
            .unwrap();
    }
}
