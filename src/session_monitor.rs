//! Periodic cleanup of expired auth state.
//!
//! `purge_expired_sessions` and `purge_expired_pending_oauth2` were only ever
//! called at startup. That bounds the tables at "however much accumulated
//! since the last restart", which was fine when restarts were frequent but
//! leaves a long-lived process growing both indefinitely — sessions live 30
//! days, so a row can outlive its usefulness by weeks while every request's
//! token lookup still walks past it.
//!
//! Both purges are plain `DELETE ... WHERE created_at <= ?`, so running one
//! hourly costs nothing when there is nothing to remove.

use std::sync::Arc;
use std::time::Duration;

use rusqlite::Connection;
use tokio::sync::Mutex;
use tracing::error;

use crate::models::session::{purge_expired_pending_oauth2, purge_expired_sessions};

/// Far shorter than the 30-day session lifetime, and far longer than the
/// 15-minute pending-OAuth2 lifetime, so neither table drifts far past its
/// TTL without the sweep being a meaningful load.
const PURGE_INTERVAL: Duration = Duration::from_secs(60 * 60);

pub fn start(connection: Arc<Mutex<Connection>>) {
    crate::supervised_task::spawn_supervised("session_monitor", move || {
        let connection = connection.clone();
        async move {
            loop {
                tokio::time::sleep(PURGE_INTERVAL).await;

                if let Err(err) = purge_expired_sessions(connection.clone()).await {
                    error!("session_monitor: failed to purge expired sessions: {err:?}");
                }
                if let Err(err) = purge_expired_pending_oauth2(connection.clone()).await {
                    error!("session_monitor: failed to purge pending oauth2: {err:?}");
                }
            }
        }
    });
}
