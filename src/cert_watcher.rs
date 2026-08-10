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
        watch(config.clone(), cert.clone(), key.clone(), CHECK_INTERVAL)
    });
}

/// The watch loop, with the poll interval injected so tests can drive it
/// without waiting an hour. Never returns.
async fn watch(config: RustlsConfig, cert: PathBuf, key: PathBuf, interval: Duration) {
    // Seeded before the first sleep so startup does not count as a change and
    // trigger a pointless reload.
    let mut last_seen = modified_at(&cert);
    loop {
        tokio::time::sleep(interval).await;

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
                // Deliberately does not advance `last_seen`, so a half-written
                // file is retried on the next tick rather than being treated
                // as handled.
                error!(
                    "cert_watcher: failed to reload {}: {err}; \
                     continuing with the previously loaded certificate",
                    cert.display()
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    /// Identifies which `ServerConfig` is live. `reload_from_pem_file` stores
    /// a freshly allocated one, so a change here means a swap happened.
    fn live_config_id(config: &RustlsConfig) -> usize {
        Arc::as_ptr(&config.get_inner()) as usize
    }

    /// A self-signed cert/key PEM pair. `subject_alt_names` varies the
    /// contents so successive certs are genuinely different files.
    fn cert_and_key(name: &str) -> (String, String) {
        let cert = rcgen::generate_simple_self_signed(vec![name.to_string()])
            .expect("generate self-signed cert");
        (cert.cert.pem(), cert.signing_key.serialize_pem())
    }

    struct Fixture {
        _dir: tempfile::TempDir,
        cert: PathBuf,
        key: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let dir = tempfile::tempdir().expect("tempdir");
            let (cert_pem, key_pem) = cert_and_key("salsa.test");
            let cert = dir.path().join("fullchain.pem");
            let key = dir.path().join("privkey.pem");
            std::fs::write(&cert, cert_pem).expect("write cert");
            std::fs::write(&key, key_pem).expect("write key");
            Fixture {
                _dir: dir,
                cert,
                key,
            }
        }

        /// Overwrite the pair the way certbot would on renewal, bumping mtime
        /// so the watcher notices. `corrupt` truncates the cert to simulate a
        /// half-written file.
        fn renew(&self, corrupt: bool) {
            let (cert_pem, key_pem) = cert_and_key("salsa.test");
            let cert_pem = if corrupt {
                cert_pem[..cert_pem.len() / 2].to_string()
            } else {
                cert_pem
            };
            std::fs::write(&self.cert, cert_pem).expect("write cert");
            std::fs::write(&self.key, key_pem).expect("write key");
            // Coarse filesystem mtime resolution would otherwise make the
            // rewrite indistinguishable from the original.
            let future = std::time::SystemTime::now() + Duration::from_secs(10);
            filetime::set_file_mtime(&self.cert, filetime::FileTime::from_system_time(future))
                .expect("bump mtime");
        }

        async fn config(&self) -> RustlsConfig {
            RustlsConfig::from_pem_file(&self.cert, &self.key)
                .await
                .expect("initial config should load")
        }
    }

    fn install_crypto_provider() {
        use std::sync::Once;
        static ONCE: Once = Once::new();
        ONCE.call_once(|| {
            let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        });
    }

    const TICK: Duration = Duration::from_millis(20);

    /// The point of the whole module: a certificate replaced on disk is picked
    /// up without the process restarting.
    #[tokio::test]
    async fn renewed_certificate_is_loaded_without_a_restart() {
        install_crypto_provider();
        let fixture = Fixture::new();
        let config = fixture.config().await;
        let before = live_config_id(&config);

        let watcher = tokio::spawn(watch(
            config.clone(),
            fixture.cert.clone(),
            fixture.key.clone(),
            TICK,
        ));
        // Let the watcher seed its baseline before renewing, mirroring the
        // real sequence (server running, certbot renews later). Without this
        // the spawn can be polled after the renewal and seed from the new
        // file, leaving nothing to detect.
        tokio::time::sleep(TICK * 3).await;

        fixture.renew(false);
        tokio::time::sleep(TICK * 10).await;
        watcher.abort();

        assert_ne!(
            live_config_id(&config),
            before,
            "watcher should have swapped in the renewed certificate"
        );
    }

    /// An untouched certificate must not be reloaded: a swap per tick would
    /// churn the config for no reason.
    #[tokio::test]
    async fn unchanged_certificate_is_not_reloaded() {
        install_crypto_provider();
        let fixture = Fixture::new();
        let config = fixture.config().await;
        let before = live_config_id(&config);

        let watcher = tokio::spawn(watch(
            config.clone(),
            fixture.cert.clone(),
            fixture.key.clone(),
            TICK,
        ));
        tokio::time::sleep(TICK * 10).await;
        watcher.abort();

        assert_eq!(
            live_config_id(&config),
            before,
            "watcher should not reload a certificate that has not changed"
        );
    }

    /// A half-written file must leave the running config alone — the site
    /// keeps serving the old, still-valid certificate — and must be retried
    /// once the file is complete, which is why `last_seen` is not advanced on
    /// failure.
    #[tokio::test]
    async fn corrupt_certificate_is_refused_then_retried_when_fixed() {
        install_crypto_provider();
        let fixture = Fixture::new();
        let config = fixture.config().await;
        let before = live_config_id(&config);

        let watcher = tokio::spawn(watch(
            config.clone(),
            fixture.cert.clone(),
            fixture.key.clone(),
            TICK,
        ));
        tokio::time::sleep(TICK * 3).await;

        fixture.renew(true);
        tokio::time::sleep(TICK * 10).await;
        assert_eq!(
            live_config_id(&config),
            before,
            "a truncated certificate must not replace the live config"
        );

        // certbot finishes writing; the watcher must still pick it up.
        fixture.renew(false);
        tokio::time::sleep(TICK * 10).await;
        watcher.abort();

        assert_ne!(
            live_config_id(&config),
            before,
            "watcher should retry after a failed reload, not give up"
        );
    }

    /// A certificate that vanishes (mid-renewal, or a broken symlink) must not
    /// take the server down or clear the loaded config.
    #[tokio::test]
    async fn missing_certificate_leaves_the_live_config_alone() {
        install_crypto_provider();
        let fixture = Fixture::new();
        let config = fixture.config().await;
        let before = live_config_id(&config);

        let watcher = tokio::spawn(watch(
            config.clone(),
            fixture.cert.clone(),
            fixture.key.clone(),
            TICK,
        ));
        std::fs::remove_file(&fixture.cert).expect("remove cert");
        tokio::time::sleep(TICK * 10).await;

        assert_eq!(
            live_config_id(&config),
            before,
            "a missing certificate must not disturb the live config"
        );
        assert!(!watcher.is_finished(), "watcher must keep running");
        watcher.abort();
    }
}
