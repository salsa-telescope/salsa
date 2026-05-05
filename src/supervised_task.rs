//! Supervised long-running background tasks.
//!
//! Without supervision, a `tokio::spawn(async move { loop { ... } })` whose
//! body panics dies silently and the corresponding feature stays broken
//! until the next process restart. This wrapper catches panics and restarts
//! the task after a short backoff.

use std::future::Future;
use std::time::Duration;
use tracing::error;

const RESTART_BACKOFF: Duration = Duration::from_secs(5);

/// Spawn `make_future` as a background task. If it panics or returns, log
/// the cause and restart it after `RESTART_BACKOFF`.
pub fn spawn_supervised<F, Fut>(name: &'static str, make_future: F)
where
    F: Fn() -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    tokio::spawn(async move {
        loop {
            let handle = tokio::spawn(make_future());
            match handle.await {
                Ok(()) => {
                    error!("{name}: task exited unexpectedly; restarting in {RESTART_BACKOFF:?}");
                }
                Err(e) if e.is_panic() => {
                    error!("{name}: task panicked ({e:?}); restarting in {RESTART_BACKOFF:?}");
                }
                Err(e) => {
                    error!("{name}: task aborted ({e:?}); restarting in {RESTART_BACKOFF:?}");
                }
            }
            tokio::time::sleep(RESTART_BACKOFF).await;
        }
    });
}
