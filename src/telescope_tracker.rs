use crate::coords::{Direction, Location};
use crate::coords::{horizontal_from_equatorial, horizontal_from_galactic};
use crate::models::telescope_types::{TelescopeError, TelescopeStatus, TelescopeTarget};
use crate::telescope_controller::{TelescopeCommand, TelescopeController, TelescopeResponse};
use chrono::{DateTime, TimeDelta, Utc};
use log::{debug, error, info};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::time::{Instant, sleep_until};

pub const LOWEST_ALLOWED_ELEVATION: f64 = 5.0f64 / 180.0f64 * std::f64::consts::PI;

pub struct TelescopeTrackerInfo {
    pub target: TelescopeTarget,
    pub commanded_horizontal: Option<Direction>,
    pub current_horizontal: Direction,
    pub status: TelescopeStatus,
    pub most_recent_error: Option<TelescopeError>,
}

pub struct TelescopeTracker {
    state: Arc<Mutex<TelescopeTrackerState>>,
    task: Arc<tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>>,
}

impl TelescopeTracker {
    pub fn new(controller_address: String) -> TelescopeTracker {
        let state = Arc::new(Mutex::new(TelescopeTrackerState {
            // TODO: This should be configurable, probably per telescope
            target: TelescopeTarget::Galactic {
                longitude: 140_f64.to_radians(),
                latitude: 0.0,
            },
            commanded_horizontal: None,
            stop_tracking_time: None,
            current_direction: None,
            most_recent_error: None,
            should_restart: false,
            quit: false,
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
    ) -> Result<TelescopeTarget, TelescopeError> {
        let mut state = self.state.lock().unwrap();
        assert!(!state.quit);
        state.target = target;
        state.stop_tracking_time = Some(Utc::now() + TimeDelta::seconds(10));
        Ok(target)
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
        let current_horizontal = match state.current_direction {
            Some(current_horizontal) => current_horizontal,
            None => return Err(TelescopeError::TelescopeNotConnected),
        };
        let commanded_horizontal = self.commanded_horizontal();
        let status = match commanded_horizontal {
            Some(commanded_horizontal) => {
                // Check if more than 2 tolerances off, if so we are not tracking anymore
                if directions_are_close(commanded_horizontal, current_horizontal, 2.0) {
                    TelescopeStatus::Tracking
                } else {
                    TelescopeStatus::Slewing
                }
            }
            None => TelescopeStatus::Idle,
        };
        let (target, most_recent_error) = { (state.target, state.most_recent_error.clone()) };
        Ok(TelescopeTrackerInfo {
            target,
            current_horizontal,
            commanded_horizontal,
            status,
            most_recent_error,
        })
    }

    pub fn direction(&self) -> Result<Direction, TelescopeError> {
        let state = self.state.lock().unwrap();
        assert!(!state.quit);
        match state.current_direction {
            Some(current_direction) => Ok(current_direction),
            None => Err(TelescopeError::TelescopeNotConnected),
        }
    }

    #[allow(dead_code)]
    pub fn target(&self) -> Result<TelescopeTarget, TelescopeError> {
        let state = self.state.lock().unwrap();
        assert!(!state.quit);
        Ok(state.target)
    }

    fn commanded_horizontal(&self) -> Option<Direction> {
        let state = self.state.lock().unwrap();
        assert!(!state.quit);
        state.commanded_horizontal
    }
}

struct TelescopeTrackerState {
    target: TelescopeTarget,
    commanded_horizontal: Option<Direction>,
    stop_tracking_time: Option<DateTime<Utc>>,
    current_direction: Option<Direction>,
    most_recent_error: Option<TelescopeError>,
    should_restart: bool,
    quit: bool,
}

async fn tracker_task_function(
    state: Arc<Mutex<TelescopeTrackerState>>,
    controller_address: String,
) {
    let mut connection_established = false;

    while !state.lock().unwrap().quit {
        debug!("tick");
        // 1 Hz update freq
        sleep_until(Instant::now() + Duration::from_secs(1)).await;

        let mut controller = match TelescopeController::connect(&controller_address) {
            Ok(controller) => controller,
            Err(err) => {
                error!("Failed to connect to contoller for {}", err);
                state.lock().unwrap().most_recent_error = Some(err);
                continue;
            }
        };

        if !connection_established {
            let mut state_guard = state.lock().unwrap();
            state_guard.most_recent_error = controller.execute(TelescopeCommand::Stop).err();
            state_guard.commanded_horizontal = None;
            connection_established = true;
        }

        debug!("checking if should stop tracking");
        if let Some(stop_telescope_time) = state.lock().unwrap().stop_tracking_time
            && stop_telescope_time < Utc::now()
        {
            let mut state_guard = state.lock().unwrap();
            state_guard.commanded_horizontal = None;
            state_guard.stop_tracking_time = None;
            debug!("stopped tracking");
        }
        debug!("done checking");

        if state.lock().unwrap().should_restart {
            info!("Controller for restarting");
            state.lock().unwrap().most_recent_error =
                controller.execute(TelescopeCommand::Restart).err();
            connection_established = false;
            sleep_until(Instant::now() + Duration::from_secs(10)).await;
            state.lock().unwrap().should_restart = false;
            continue;
        }

        let res = update_direction(&mut state.lock().unwrap(), Utc::now(), &mut controller);
        state.lock().unwrap().most_recent_error = res.err();
    }
}

fn update_direction(
    state: &mut TelescopeTrackerState,
    when: DateTime<Utc>,
    controller: &mut TelescopeController,
) -> Result<(), TelescopeError> {
    // FIXME: How do we handle static configuration like this?
    let location = Location {
        longitude: 0.20802143022, //(11.0+55.0/60.0+7.5/3600.0) * PI / 180.0. Sign positive, handled in gmst calc
        latitude: 1.00170457462,  //(57.0+23.0/60.0+36.4/3600.0) * PI / 180.0
    };
    let target_horizontal = calculate_target_horizontal(state.target, location, when);
    let current_horizontal = match controller.execute(TelescopeCommand::GetDirection)? {
        TelescopeResponse::CurrentDirection(direction) => Ok(direction),
        _ => Err(TelescopeError::TelescopeIOError(
            "Telescope did not respond with current direction".to_string(),
        )),
    }?;
    state.current_direction = Some(current_horizontal);

    match target_horizontal {
        Some(target_horizontal) => {
            // FIXME: How to handle static configuration like this?
            if target_horizontal.elevation < LOWEST_ALLOWED_ELEVATION {
                state.most_recent_error = Some(TelescopeError::TargetBelowHorizon);
                state.commanded_horizontal = None;
                return Err(TelescopeError::TargetBelowHorizon);
            }

            state.commanded_horizontal = Some(target_horizontal);

            // Check if more than 1 tolerance off, if so we need to send track command
            if !directions_are_close(target_horizontal, current_horizontal, 1.0) {
                controller.execute(TelescopeCommand::SetDirection(target_horizontal))?;
            }

            Ok(())
        }
        None => {
            if state.commanded_horizontal.is_some() {
                controller.execute(TelescopeCommand::Stop)?;
                state.commanded_horizontal = None;
            }
            Ok(())
        }
    }
}

fn calculate_target_horizontal(
    target: TelescopeTarget,
    location: Location,
    when: DateTime<Utc>,
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
        TelescopeTarget::Parked => None,
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
