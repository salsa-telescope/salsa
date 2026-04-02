use crate::coords::{Direction, Location};
use crate::coords::{horizontal_from_equatorial, horizontal_from_galactic, horizontal_from_sun};
use crate::models::telescope_types::{TelescopeError, TelescopeStatus, TelescopeTarget};
use crate::telescope_controller::{TelescopeCommand, TelescopeController, TelescopeResponse};
use crate::tle_cache::TleCacheHandle;
use chrono::{DateTime, Utc};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::time::{Instant, sleep_until};
use tracing::{debug, error, info, warn};

pub struct TelescopeTrackerInfo {
    pub target: Option<TelescopeTarget>,
    pub commanded_horizontal: Option<Direction>,
    pub current_horizontal: Option<Direction>,
    pub status: TelescopeStatus,
    pub most_recent_error: Option<TelescopeError>,
    pub az_offset_rad: f64,
    pub el_offset_rad: f64,
}

pub struct TelescopeTracker {
    state: Arc<Mutex<TelescopeTrackerState>>,
    task: Arc<tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>>,
}

impl TelescopeTracker {
    pub fn new(
        controller_address: String,
        location: Location,
        min_elevation_rad: f64,
        max_elevation_rad: f64,
        tle_cache: TleCacheHandle,
    ) -> TelescopeTracker {
        let state = Arc::new(Mutex::new(TelescopeTrackerState {
            target: None,
            az_offset_rad: 0.0,
            el_offset_rad: 0.0,
            commanded_horizontal: None,
            current_direction: None,
            most_recent_error: None,
            should_restart: false,
            quit: false,
            tle_cache: tle_cache.clone(),
            location,
            min_elevation_rad,
            max_elevation_rad,
        }));
        let task = tokio::spawn(tracker_task_function(state.clone(), controller_address));
        TelescopeTracker {
            state,
            task: Arc::new(tokio::sync::Mutex::new(Some(task))),
        }
    }

    pub async fn shutdown(&self) {
        {
            let mut state = self.state.lock().unwrap();
            state.quit = true;
        }
        self.task
            .lock()
            .await
            .take()
            .expect("Should be an active task")
            .await
            .expect("Joining task should work");
    }

    pub fn set_target(
        &mut self,
        target: TelescopeTarget,
        az_offset_rad: f64,
        el_offset_rad: f64,
    ) -> Result<TelescopeTarget, TelescopeError> {
        let mut state = self.state.lock().unwrap();
        assert!(!state.quit);
        state.target = Some(target);
        state.az_offset_rad = az_offset_rad;
        state.el_offset_rad = el_offset_rad;
        Ok(target)
    }

    pub fn stop(&mut self) -> Result<(), TelescopeError> {
        let mut state = self.state.lock().unwrap();
        assert!(!state.quit);
        state.target = None;
        Ok(())
    }

    #[allow(dead_code)]
    pub fn restart(&self) {
        let mut state = self.state.lock().unwrap();
        assert!(!state.quit);
        state.should_restart = true;
    }

    pub fn info(&self) -> Result<TelescopeTrackerInfo, TelescopeError> {
        let state = self.state.lock().unwrap();
        assert!(!state.quit);
        let current_horizontal = state.current_direction;
        let commanded_horizontal = state.commanded_horizontal;
        let status = match commanded_horizontal {
            Some(commanded_horizontal) => {
                let Some(current_horizontal) = current_horizontal else {
                    return Err(TelescopeError::TelescopeNotConnected);
                };

                // Check if more than 2 tolerances off, if so we are not tracking anymore
                if directions_are_close(commanded_horizontal, current_horizontal, 2.0) {
                    TelescopeStatus::Tracking
                } else {
                    TelescopeStatus::Slewing
                }
            }
            None => TelescopeStatus::Idle,
        };
        let (target, most_recent_error, az_offset_rad, el_offset_rad) = {
            (
                state.target,
                state.most_recent_error.clone(),
                state.az_offset_rad,
                state.el_offset_rad,
            )
        };
        Ok(TelescopeTrackerInfo {
            target,
            current_horizontal,
            commanded_horizontal,
            status,
            most_recent_error,
            az_offset_rad,
            el_offset_rad,
        })
    }
}

struct TelescopeTrackerState {
    target: Option<TelescopeTarget>,
    az_offset_rad: f64,
    el_offset_rad: f64,
    commanded_horizontal: Option<Direction>,
    current_direction: Option<Direction>,
    most_recent_error: Option<TelescopeError>,
    should_restart: bool,
    quit: bool,
    tle_cache: TleCacheHandle,
    location: Location,
    min_elevation_rad: f64,
    max_elevation_rad: f64,
}

async fn tracker_task_function(
    state: Arc<Mutex<TelescopeTrackerState>>,
    controller_address: String,
) {
    let mut controller: Option<TelescopeController> = None;
    let mut prev_target: Option<TelescopeTarget> = None;
    // The MD01 controller closes the TCP connection after responding to Stop.
    // So we send Stop once on first startup on its own connection, then reconnect
    // for all subsequent communication. On later reconnects we skip Stop.
    let mut initial_stop_done = false;

    while !state.lock().unwrap().quit {
        // 1 Hz update freq
        sleep_until(Instant::now() + Duration::from_secs(1)).await;

        let target = state.lock().unwrap().target;

        // If target just became None, send Stop to hardware
        let need_stop = prev_target.is_some() && target.is_none();
        prev_target = target;

        // Establish connection if not already connected
        if controller.is_none() {
            controller = match TelescopeController::connect(&controller_address) {
                Ok(c) => Some(c),
                Err(err) => {
                    error!(
                        "Failed to connect to controller at {}: {}",
                        &controller_address, err
                    );
                    state.lock().unwrap().most_recent_error = Some(err);
                    continue;
                }
            };

            if !initial_stop_done {
                // Send Stop once on first startup to ensure the rotor is not
                // moving from a previous session. The controller closes the TCP
                // connection after responding to Stop, so we drop this connection
                // and reconnect fresh on the next iteration for actual work.
                let ctrl = controller.as_mut().unwrap();
                match ctrl.execute(TelescopeCommand::Stop) {
                    Ok(_) => {
                        state.lock().unwrap().most_recent_error = None;
                    }
                    Err(err) => {
                        warn!("Initial stop command failed: {}", err);
                        state.lock().unwrap().most_recent_error = Some(err);
                    }
                }
                initial_stop_done = true;
                controller = None; // Reconnect fresh; controller closed after Stop
                continue;
            }

            state.lock().unwrap().commanded_horizontal = None;
        }

        let ctrl = controller.as_mut().unwrap();

        if need_stop {
            debug!("Target set to None, sending Stop to controller");
            if let Err(err) = ctrl.execute(TelescopeCommand::Stop) {
                state.lock().unwrap().most_recent_error = Some(err);
            } else {
                state.lock().unwrap().commanded_horizontal = None;
            }
            // Controller closes the TCP connection after responding to Stop,
            // so reconnect fresh on the next iteration.
            controller = None;
            continue;
        }

        if state.lock().unwrap().should_restart {
            info!("Restarting controller");
            if let Err(err) = ctrl.execute(TelescopeCommand::Restart) {
                state.lock().unwrap().most_recent_error = Some(err);
            }
            controller = None;
            sleep_until(Instant::now() + Duration::from_secs(10)).await;
            state.lock().unwrap().should_restart = false;
            continue;
        }

        let res = update_direction(&state, Utc::now(), ctrl);
        match res {
            Ok(()) => {
                state.lock().unwrap().most_recent_error = None;
            }
            Err(
                err @ (TelescopeError::TelescopeIOError(_) | TelescopeError::TelescopeNotConnected),
            ) => {
                state.lock().unwrap().most_recent_error = Some(err);
                controller = None;
            }
            Err(err) => {
                state.lock().unwrap().most_recent_error = Some(err);
            }
        }
    }
}

fn update_direction(
    state: &Arc<Mutex<TelescopeTrackerState>>,
    when: DateTime<Utc>,
    controller: &mut TelescopeController,
) -> Result<(), TelescopeError> {
    // Read target, offsets, location, elevation limits, and tle_cache from state, then release the lock
    let (
        target,
        az_offset_rad,
        el_offset_rad,
        location,
        min_elevation_rad,
        max_elevation_rad,
        tle_cache,
    ) = {
        let state_guard = state.lock().unwrap();
        (
            state_guard.target,
            state_guard.az_offset_rad,
            state_guard.el_offset_rad,
            state_guard.location,
            state_guard.min_elevation_rad,
            state_guard.max_elevation_rad,
            state_guard.tle_cache.clone(),
        )
    };

    let current_horizontal = match controller.execute(TelescopeCommand::GetDirection)? {
        TelescopeResponse::CurrentDirection(direction) => Ok(direction),
        _ => Err(TelescopeError::TelescopeIOError(
            "Telescope did not respond with current direction".to_string(),
        )),
    }?;

    let Some(target) = target else {
        state.lock().unwrap().current_direction = Some(current_horizontal);
        return Ok(());
    };

    let Some(raw_horizontal) = calculate_target_horizontal(target, location, when, &tle_cache)
    else {
        // Satellite not yet in TLE cache — skip this update cycle
        state.lock().unwrap().current_direction = Some(current_horizontal);
        return Ok(());
    };
    let target_horizontal = apply_offset(raw_horizontal, az_offset_rad, el_offset_rad);

    if target_horizontal.elevation < min_elevation_rad
        || target_horizontal.elevation > max_elevation_rad
    {
        let err = TelescopeError::TargetOutOfElevationRange {
            min_deg: min_elevation_rad.to_degrees(),
            max_deg: max_elevation_rad.to_degrees(),
        };
        let mut state_guard = state.lock().unwrap();
        state_guard.current_direction = Some(current_horizontal);
        state_guard.most_recent_error = Some(err.clone());
        state_guard.commanded_horizontal = None;
        return Err(err);
    }

    // Check if more than 1 tolerance off, if so we need to send track command
    if !directions_are_close(target_horizontal, current_horizontal, 1.0) {
        controller.execute(TelescopeCommand::SetDirection(target_horizontal))?;
    }

    let mut state_guard = state.lock().unwrap();
    state_guard.current_direction = Some(current_horizontal);
    state_guard.commanded_horizontal = Some(target_horizontal);

    Ok(())
}

fn calculate_target_horizontal(
    target: TelescopeTarget,
    location: Location,
    when: DateTime<Utc>,
    tle_cache: &TleCacheHandle,
) -> Option<Direction> {
    match target {
        TelescopeTarget::Equatorial {
            right_ascension: ra,
            declination: dec,
        } => Some(horizontal_from_equatorial(location, when, ra, dec)),
        TelescopeTarget::Galactic {
            longitude: l,
            latitude: b,
        } => Some(horizontal_from_galactic(location, when, l, b)),
        TelescopeTarget::Horizontal {
            azimuth: az,
            elevation: el,
        } => Some(Direction {
            azimuth: az,
            elevation: el,
        }),
        TelescopeTarget::Sun => Some(horizontal_from_sun(location, when)),
        TelescopeTarget::Satellite { norad_id } => {
            tle_cache.satellite_direction(norad_id, location, when)
        }
    }
}

fn apply_offset(dir: Direction, az_offset_rad: f64, el_offset_rad: f64) -> Direction {
    let full_circle = 2.0 * std::f64::consts::PI;
    Direction {
        azimuth: ((dir.azimuth + az_offset_rad) % full_circle + full_circle) % full_circle,
        elevation: dir.elevation + el_offset_rad,
    }
}

fn directions_are_close(a: Direction, b: Direction, tol: f64) -> bool {
    // The salsa telescope works with a precision of 0.1 degrees
    // We want to send new commands whenever we exceed this tolerance
    // but to report tracking status we allow more, so that we do not flip
    // status between tracking/slewing (e.g. due to control unit rounding errors)
    // Therefore we have the "tol" multiplier here, which scales the allowed error.
    let epsilon = tol * 0.1_f64.to_radians();
    (a.azimuth - b.azimuth).abs() < epsilon && (a.elevation - b.elevation).abs() < epsilon
}
