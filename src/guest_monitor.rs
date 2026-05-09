//! Lifecycle supervisor for active guest sessions.
//!
//! Three end conditions, evaluated once per tick:
//!   * **Idle release** — no telescope command in `GUEST_IDLE_RELEASE_SECS`.
//!   * **Hard ceiling** — wall-clock time since start exceeds
//!     `GUEST_SESSION_HARD_CEILING_SECS`.
//!   * **Preempted** — a real (non-guest) booking has become active on the
//!     same telescope. The guest must yield immediately so the rightful
//!     holder can use their slot.
//!
//! When any condition fires we run the same telescope cleanup as
//! `booking_monitor` does at handover: stop integration, stop slewing,
//! clear cached measurements. Then mark the guest_session row ended with
//! the appropriate reason. `GuestSession::end` also deletes the auth
//! session row, so the cookie cleanly reverts to anonymous on the next
//! request.

use std::time::Duration;

use chrono::Utc;
use tracing::{error, info};

use crate::app::AppState;
use crate::models::booking::Booking;
use crate::models::guest::{
    EndReason, GUEST_IDLE_RELEASE_SECS, GUEST_SESSION_HARD_CEILING_SECS, GuestSession,
};
use crate::models::user::User;
use crate::routes::observe::stop_and_save_observation;

/// Tick interval. Short enough that a real booking starting at hour boundary
/// preempts the guest within a few seconds; long enough that the periodic
/// `fetch_all_active` query doesn't pile up against the SQLite mutex.
const TICK_INTERVAL: Duration = Duration::from_secs(5);

pub fn start(state: AppState) {
    crate::supervised_task::spawn_supervised("guest_monitor", move || {
        let state = state.clone();
        async move {
            loop {
                tokio::time::sleep(TICK_INTERVAL).await;

                let active_guests =
                    match GuestSession::fetch_all_active(state.database_connection.clone()).await {
                        Ok(g) => g,
                        Err(err) => {
                            error!("guest_monitor: failed to fetch active guests: {err:?}");
                            continue;
                        }
                    };
                if active_guests.is_empty() {
                    continue;
                }

                let active_bookings =
                    match Booking::fetch_active(state.database_connection.clone()).await {
                        Ok(b) => b,
                        Err(err) => {
                            error!("guest_monitor: failed to fetch active bookings: {err:?}");
                            continue;
                        }
                    };

                let now = Utc::now();
                for guest in active_guests {
                    let reason = if active_bookings
                        .iter()
                        .any(|b| b.telescope_name == guest.telescope_id)
                    {
                        Some(EndReason::Preempted)
                    } else if (now - guest.last_activity_at).num_seconds()
                        >= GUEST_IDLE_RELEASE_SECS
                    {
                        Some(EndReason::Idle)
                    } else if (now - guest.started_at).num_seconds()
                        >= GUEST_SESSION_HARD_CEILING_SECS
                    {
                        Some(EndReason::Ceiling)
                    } else {
                        None
                    };
                    let Some(reason) = reason else { continue };

                    end_session(&state, &guest, reason).await;
                }
            }
        }
    });
}

async fn end_session(state: &AppState, guest: &GuestSession, reason: EndReason) {
    let synthetic_user = User {
        id: guest.user_id,
        name: format!("guest-{}", guest.user_id),
        provider: "guest".to_string(),
        is_admin: false,
    };
    if let Some(telescope) = state.telescopes.get(&guest.telescope_id).await {
        stop_and_save_observation(
            telescope.as_ref(),
            state.database_connection.clone(),
            &synthetic_user,
            &state.tle_cache,
        )
        .await;
        if let Err(err) = telescope.stop().await {
            error!(
                "guest_monitor: failed to stop telescope {}: {err:?}",
                guest.telescope_id
            );
        }
        telescope.clear_measurements().await;
    }
    if let Err(err) =
        GuestSession::end(state.database_connection.clone(), guest.id, reason.clone()).await
    {
        error!(
            "guest_monitor: failed to mark guest session {} ended ({reason:?}): {err:?}",
            guest.id
        );
        return;
    }
    info!(
        "guest_monitor: guest session {} on {} ended ({reason:?})",
        guest.id, guest.telescope_id
    );
}
