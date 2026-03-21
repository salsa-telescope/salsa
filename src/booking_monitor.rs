use std::collections::HashMap;
use std::time::Duration;

use chrono::Utc;
use log::error;

use crate::app::AppState;
use crate::models::booking::Booking;
use crate::models::telescope_types::ReceiverConfiguration;
use crate::models::user::User;
use crate::routes::observe::save_latest_observation;

pub fn start(state: AppState) {
    tokio::spawn(async move {
        // Maps telescope_name -> User of the last seen active booking holder.
        let mut active_users: HashMap<String, User> = HashMap::new();
        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;

            let bookings = match Booking::fetch_active(state.database_connection.clone()).await {
                Ok(b) => b,
                Err(err) => {
                    error!("Booking monitor: failed to fetch active bookings: {err:?}");
                    continue;
                }
            };

            let telescope_names = state.telescopes.get_names().await;
            for telescope_name in &telescope_names {
                let now = Utc::now();
                let active_booking = bookings
                    .iter()
                    .find(|b| b.telescope_name == *telescope_name && b.active_at(&now));

                let current_user = active_booking.map(|b| User {
                    id: b.user_id,
                    name: b.user_name.clone(),
                    provider: b.user_provider.clone(),
                    is_admin: false,
                });
                let previous_user = active_users.get(telescope_name).cloned();

                let should_stop = match (&current_user, &previous_user) {
                    (None, Some(_)) => true,
                    (Some(curr), Some(prev)) if curr.id != prev.id => true,
                    _ => false,
                };

                if !should_stop {
                    match current_user {
                        Some(u) => {
                            active_users.insert(telescope_name.clone(), u);
                        }
                        None => {
                            active_users.remove(telescope_name);
                        }
                    }
                    continue;
                }

                let Some(prev_user) = previous_user else {
                    continue;
                };

                let telescope = match state.telescopes.get(telescope_name).await {
                    Some(t) => t,
                    None => continue,
                };

                save_latest_observation(
                    state.database_connection.clone(),
                    &prev_user,
                    telescope.as_ref(),
                )
                .await;

                let mut stop_ok = true;

                if let Err(err) = telescope
                    .set_receiver_configuration(ReceiverConfiguration {
                        integrate: false,
                        ..Default::default()
                    })
                    .await
                {
                    error!("Booking monitor: failed to stop integration: {err:?}");
                    stop_ok = false;
                }

                if let Err(err) = telescope.stop().await {
                    error!("Booking monitor: failed to stop telescope: {err:?}");
                    stop_ok = false;
                }

                if stop_ok {
                    match current_user {
                        Some(u) => {
                            active_users.insert(telescope_name.clone(), u);
                        }
                        None => {
                            active_users.remove(telescope_name);
                        }
                    }
                }
            }
        }
    });
}
