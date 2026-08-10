//! Picks up renewed TLS certificates without restarting the server.
//!
//! Previously the only way the process saw a renewed certificate was to be
//! restarted, which certbot did via `pre_hook = systemctl stop salsa.service`.
//! That hook existed for two reasons — freeing port 80 for the `--standalone`
//! HTTP-01 challenge, and getting the new certificate loaded — and it cost a
//! hard restart roughly every 60 days, at whatever hour certbot's timer fired.
//! Landing mid-booking, that kills the running integration.
//!
//! With the challenge now served from the redirect app (see
//! `app::create_redirect_app`) certbot no longer needs the port, and this
//! watcher covers the reload, so the hooks can go.
//!
//! Polling rather than SIGHUP because it needs no coordination with anything
//! on the host: no unit-file `ExecReload`, no renewal-conf hook to be lost the
//! next time the server is rebuilt. An hour of latency does not matter —
//! certbot renews with ~30 days of validity left, so the certificate being
//! served in the meantime is nowhere near expiry.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use axum_server::tls_rustls::RustlsConfig;
use tracing::{error, info, warn};

const CHECK_INTERVAL: Duration = Duration::from_secs(60 * 60);

/// Last-modified time of `path`, or `None` if it cannot be read.
///
/// Follows symlinks deliberately: certbot keeps the real files in
/// `archive/` and re-points the `live/` symlink at renewal, so it is the
/// target's mtime that moves.
fn modified_at(path: &Path) -> Option<SystemTime> {
    match std::fs::metadata(path) {
        Ok(meta) => meta.modified().ok(),
        Err(err) => {
            warn!("cert_watcher: cannot stat {}: {err}", path.display());
            None
        }
    }
}

/// Watch `cert` and reload `config` from disk whenever it changes.
///
/// Reloading is safe by construction: `reload_from_pem_file` parses and
/// validates the files into a new `ServerConfig` before swapping it in, so a
/// truncated or malformed certificate leaves the running config untouched and
/// the site keeps serving on the old (still valid) one.
pub fn start(config: RustlsConfig, cert: PathBuf, key: PathBuf) {
    crate::supervised_task::spawn_supervised("cert_watcher", move || {
        let config = config.clone();
        let cert = cert.clone();
        let key = key.clone();
        async move {
            // Seeded before the first sleep so startup does not count as a
            // change and trigger a pointless reload.
            let mut last_seen = modified_at(&cert);
            loop {
                tokio::time::sleep(CHECK_INTERVAL).await;

                let current = modified_at(&cert);
                if current.is_none() || current == last_seen {
                    continue;
                }

                match config.reload_from_pem_file(&cert, &key).await {
                    Ok(()) => {
                        info!(
                            "cert_watcher: reloaded TLS certificate from {}",
                            cert.display()
                        );
                        last_seen = current;
                    }
                    Err(err) => {
                        // Deliberately does not advance `last_seen`, so a
                        // half-written file is retried on the next tick rather
                        // than being treated as handled.
                        error!(
                            "cert_watcher: failed to reload {}: {err}; \
                             continuing with the previously loaded certificate",
                            cert.display()
                        );
                    }
                }
            }
        }
    });
}
