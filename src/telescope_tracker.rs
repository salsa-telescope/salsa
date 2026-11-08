use crate::coords::{Direction, Location};
use crate::coords::{horizontal_from_equatorial, horizontal_from_galactic, horizontal_from_sun};
use crate::models::telescope_types::{
    CalibrationResult, TelescopeError, TelescopeStatus, TelescopeTarget,
};
use crate::telescope_controller::{TelescopeCommand, TelescopeController, TelescopeResponse};
use crate::tle_cache::TleCacheHandle;
use chrono::{DateTime, Utc};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::time::{Instant, sleep_until};
use tracing::{debug, error, info, warn};

/// How long the antenna may sit still while commanded somewhere else before
/// it is reported as not moving. The controller answers position requests
/// normally even when the motors do not run (no power, manual mode, a limit
/// or a mechanical fault), so without this check a stuck antenna would just
/// show "Slewing" forever with no error.
const STALL_TIMEOUT: Duration = if cfg!(test) {
    Duration::from_secs(2)
} else {
    Duration::from_secs(10)
};
/// How long to leave the controller alone after a restart command. It
/// restarts 5 s after acknowledging.
const RESTART_SETTLE: Duration = if cfg!(test) {
    Duration::from_secs(2)
} else {
    Duration::from_secs(10)
};
/// Smaller position changes do not count as movement. The rotor resolution
/// is 0.1°, so this is a couple of encoder steps.
const STALL_MIN_MOVEMENT_DEG: f64 = 0.2;
/// Only check for a stall while the antenna is at least this far from the
/// commanded position. Closer than that it is tracking, and the rotor may
/// legitimately stay put on small errors.
const STALL_MIN_ERROR_DEG: f64 = 1.0;

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
            // Not connected until a connection attempt says otherwise. Left as
            // `None`, "no error yet" was indistinguishable from "connected",
            // so startup logged the controller as connected before the first
            // attempt had even been made — and then immediately as lost.
            most_recent_error: Some(TelescopeError::TelescopeNotConnected),
            pending_restart: None,
            pending_calibration: None,
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
        if state.quit {
            return Err(TelescopeError::TelescopeNotConnected);
        }
        // Validate elevation at set time when we can compute the target's
        // current horizontal. Satellites without a TLE in cache are skipped —
        // the periodic update loop will surface the error once the TLE arrives.
        if let Some(raw) =
            calculate_target_horizontal(target, state.location, Utc::now(), &state.tle_cache)
        {
            let horizontal = apply_offset(raw, az_offset_rad, el_offset_rad);
            if horizontal.elevation < state.min_elevation_rad
                || horizontal.elevation > state.max_elevation_rad
            {
                return Err(TelescopeError::TargetOutOfElevationRange {
                    min_deg: state.min_elevation_rad.to_degrees(),
                    max_deg: state.max_elevation_rad.to_degrees(),
                });
            }
        }
        state.target = Some(target);
        state.az_offset_rad = az_offset_rad;
        state.el_offset_rad = el_offset_rad;
        Ok(target)
    }

    pub fn stop(&mut self) -> Result<(), TelescopeError> {
        let mut state = self.state.lock().unwrap();
        if state.quit {
            return Err(TelescopeError::TelescopeNotConnected);
        }
        state.target = None;
        Ok(())
    }

    /// Request a pointing calibration: rewrite the controller's stored
    /// position so its reported direction decreases by the given offsets
    /// (the observing offsets at which the peak of a strong source was
    /// found). The command is executed by the tracker task on its next
    /// cycle; await the returned receiver for the outcome. Refused while
    /// a target is being tracked or another calibration is pending.
    pub fn request_calibration(
        &self,
        az_offset_rad: f64,
        el_offset_rad: f64,
    ) -> Result<
        tokio::sync::oneshot::Receiver<Result<CalibrationResult, TelescopeError>>,
        TelescopeError,
    > {
        let mut state = self.state.lock().unwrap();
        if state.quit {
            return Err(TelescopeError::TelescopeNotConnected);
        }
        if state.target.is_some() || state.pending_calibration.is_some() {
            return Err(TelescopeError::TelescopeBusy);
        }
        let (responder, receiver) = tokio::sync::oneshot::channel();
        state.pending_calibration = Some(PendingCalibration {
            az_offset_rad,
            el_offset_rad,
            responder,
        });
        Ok(receiver)
    }

    /// Request a restart of the rotor controller (ROTn_CMD_RESTART_DEVICE).
    /// This clears a latched controller error such as the MD-01's TIME OUT,
    /// which stops the motors while position queries keep working. The
    /// command is sent by the tracker task on its next cycle; await the
    /// returned receiver for whether the controller acknowledged it. Any
    /// target is kept, so tracking resumes once the controller is back.
    pub fn request_restart(
        &self,
    ) -> Result<tokio::sync::oneshot::Receiver<Result<(), TelescopeError>>, TelescopeError> {
        let mut state = self.state.lock().unwrap();
        if state.quit {
            return Err(TelescopeError::TelescopeNotConnected);
        }
        if state.pending_restart.is_some() {
            return Err(TelescopeError::TelescopeBusy);
        }
        let (responder, receiver) = tokio::sync::oneshot::channel();
        state.pending_restart = Some(responder);
        Ok(receiver)
    }

    pub fn info(&self) -> Result<TelescopeTrackerInfo, TelescopeError> {
        let state = self.state.lock().unwrap();
        if state.quit {
            return Err(TelescopeError::TelescopeNotConnected);
        }
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
    /// Set while a restart is requested and not yet sent; answered with the
    /// outcome of sending the command.
    pending_restart: Option<tokio::sync::oneshot::Sender<Result<(), TelescopeError>>>,
    pending_calibration: Option<PendingCalibration>,
    quit: bool,
    tle_cache: TleCacheHandle,
    location: Location,
    min_elevation_rad: f64,
    max_elevation_rad: f64,
}

struct PendingCalibration {
    az_offset_rad: f64,
    el_offset_rad: f64,
    responder: tokio::sync::oneshot::Sender<Result<CalibrationResult, TelescopeError>>,
}

async fn tracker_task_function(
    state: Arc<Mutex<TelescopeTrackerState>>,
    controller_address: String,
) {
    let mut controller: Option<TelescopeController> = None;
    let mut prev_target: Option<TelescopeTarget> = None;
    let mut stall = StallDetector::default();
    // A stall is answered with one automatic controller restart, which
    // clears the MD-01's latched TIME OUT. It is re-armed only once the
    // antenna has reached a target again, so a rotor that is really jammed
    // is not driven against the jam over and over.
    let mut auto_restart_armed = true;
    let mut auto_restarted = false;
    // Set by any restart, manual or automatic, so the log shows whether the
    // antenna reached its target afterwards.
    let mut restarted_awaiting_target = false;
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
            stall = StallDetector::default();
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

        let pending_restart = state.lock().unwrap().pending_restart.take();
        if let Some(responder) = pending_restart {
            let result = send_restart(ctrl, &controller_address);
            // The requester may have timed out and dropped the receiver.
            let _ = responder.send(result);
            restarted_awaiting_target = true;
            controller = None;
            wait_after_restart(&state).await;
            continue;
        }

        let pending_calibration = state.lock().unwrap().pending_calibration.take();
        if let Some(pending) = pending_calibration {
            let result = perform_calibration(ctrl, pending.az_offset_rad, pending.el_offset_rad);
            if let Ok(calibration) = &result {
                info!(
                    "Calibrated controller position: az/el ({:.2}°, {:.2}°) -> ({:.2}°, {:.2}°)",
                    calibration.previous.azimuth.to_degrees(),
                    calibration.previous.elevation.to_degrees(),
                    calibration.adjusted.azimuth.to_degrees(),
                    calibration.adjusted.elevation.to_degrees(),
                );
                state.lock().unwrap().current_direction = Some(calibration.adjusted);
            }
            // The requester may have timed out and dropped the receiver;
            // nothing to do about it, the calibration already happened.
            let _ = pending.responder.send(result);
            // Reconnect fresh: the calibration response is in the legacy
            // frame format and the connection state after it is untested.
            controller = None;
            continue;
        }

        let was_stalled = stall.is_stalled();
        let res = update_direction(&state, Utc::now(), ctrl, &mut stall);
        if stall.is_stalled() && !was_stalled {
            warn!(
                "Antenna at {} has not moved for {} s although commanded to a new position; \
                 the controller answers but the rotor does not turn",
                &controller_address,
                STALL_TIMEOUT.as_secs()
            );
        } else if was_stalled && !stall.is_stalled() {
            info!("Antenna at {} is moving again", &controller_address);
        }
        let newly_stalled = stall.is_stalled() && !was_stalled;
        if res.is_ok()
            && (restarted_awaiting_target || !auto_restart_armed)
            && reached_target(&state)
        {
            if restarted_awaiting_target {
                info!(
                    "Antenna at {} reached its target after the controller restart",
                    &controller_address
                );
            }
            auto_restart_armed = true;
            auto_restarted = false;
            restarted_awaiting_target = false;
        }
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

        if newly_stalled && let Some(ctrl) = controller.as_mut() {
            if auto_restart_armed {
                warn!(
                    "Restarting controller at {} automatically to clear a possible latched \
                     error; not repeated until the antenna has reached a target",
                    &controller_address
                );
                auto_restart_armed = false;
                auto_restarted = true;
                restarted_awaiting_target = true;
                let _ = send_restart(ctrl, &controller_address);
                controller = None;
                wait_after_restart(&state).await;
            } else if auto_restarted {
                warn!(
                    "Antenna at {} is still not moving after the automatic controller restart; \
                     it needs to be checked",
                    &controller_address
                );
            }
        }
    }
}

/// Send the controller restart command and log the outcome.
fn send_restart(
    controller: &mut TelescopeController,
    controller_address: &str,
) -> Result<(), TelescopeError> {
    let result = controller.execute(TelescopeCommand::Restart).map(|_| ());
    match &result {
        Ok(()) => info!("Restart command acknowledged by controller at {controller_address}"),
        Err(err) => warn!("Restart command to controller at {controller_address} failed: {err}"),
    }
    result
}

/// Give a restarting controller time to come back before talking to it
/// again, waking each second so a server shutdown is not held up. The
/// caller drops the connection; the reconnect also resets the stall detector.
async fn wait_after_restart(state: &Arc<Mutex<TelescopeTrackerState>>) {
    for _ in 0..RESTART_SETTLE.as_secs() {
        if state.lock().unwrap().quit {
            return;
        }
        sleep_until(Instant::now() + Duration::from_secs(1)).await;
    }
}

/// Whether the antenna is on its commanded position (the Tracking status).
fn reached_target(state: &Arc<Mutex<TelescopeTrackerState>>) -> bool {
    let state = state.lock().unwrap();
    match (state.commanded_horizontal, state.current_direction) {
        (Some(commanded), Some(current)) => directions_are_close(commanded, current, 2.0),
        _ => false,
    }
}

/// Read the controller's reported position and rewrite it, reduced by the
/// measured pointing offsets, using the calibration command (which does not
/// move the rotor). The peak of a strong source was found with the observing
/// offsets applied, i.e. the controller's reported position is too high by
/// exactly those offsets.
fn perform_calibration(
    controller: &mut TelescopeController,
    az_offset_rad: f64,
    el_offset_rad: f64,
) -> Result<CalibrationResult, TelescopeError> {
    let previous = match controller.execute(TelescopeCommand::GetDirection)? {
        TelescopeResponse::CurrentDirection(direction) => direction,
        _ => {
            return Err(TelescopeError::TelescopeIOError(
                "Telescope did not respond with current direction".to_string(),
            ));
        }
    };
    let adjusted = Direction {
        azimuth: previous.azimuth - az_offset_rad,
        elevation: previous.elevation - el_offset_rad,
    };
    controller.execute(TelescopeCommand::Calibrate(adjusted))?;
    Ok(CalibrationResult { previous, adjusted })
}

fn update_direction(
    state: &Arc<Mutex<TelescopeTrackerState>>,
    when: DateTime<Utc>,
    controller: &mut TelescopeController,
    stall: &mut StallDetector,
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
        *stall = StallDetector::default();
        state.lock().unwrap().current_direction = Some(current_horizontal);
        return Ok(());
    };

    let Some(raw_horizontal) = calculate_target_horizontal(target, location, when, &tle_cache)
    else {
        // Satellite not yet in TLE cache — skip this update cycle
        *stall = StallDetector::default();
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
        *stall = StallDetector::default();
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

    let far_from_target = !directions_are_close(
        target_horizontal,
        current_horizontal,
        STALL_MIN_ERROR_DEG / 0.1,
    );
    stall.update(
        current_horizontal,
        far_from_target,
        std::time::Instant::now(),
    );

    let mut state_guard = state.lock().unwrap();
    state_guard.current_direction = Some(current_horizontal);
    state_guard.commanded_horizontal = Some(target_horizontal);
    drop(state_guard);

    if stall.is_stalled() {
        return Err(TelescopeError::TelescopeNotMoving {
            seconds: STALL_TIMEOUT.as_secs(),
        });
    }
    Ok(())
}

/// Notices an antenna that does not move although it is far from where it
/// has been told to go. Fed the reported position once per tracker cycle.
#[derive(Default)]
struct StallDetector {
    /// Position at the last observed movement, and when it was seen.
    anchor: Option<(Direction, std::time::Instant)>,
    stalled: bool,
}

impl StallDetector {
    fn update(&mut self, current: Direction, far_from_target: bool, now: std::time::Instant) {
        if !far_from_target {
            *self = StallDetector::default();
            return;
        }
        let min_movement = STALL_MIN_MOVEMENT_DEG.to_radians();
        match self.anchor {
            Some((position, since))
                if (current.azimuth - position.azimuth).abs() < min_movement
                    && (current.elevation - position.elevation).abs() < min_movement =>
            {
                self.stalled = now.duration_since(since) >= STALL_TIMEOUT;
            }
            _ => {
                self.anchor = Some((current, now));
                self.stalled = false;
            }
        }
    }

    fn is_stalled(&self) -> bool {
        self.stalled
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn deg(az: f64, el: f64) -> Direction {
        Direction {
            azimuth: az.to_radians(),
            elevation: el.to_radians(),
        }
    }

    /// A minimal MD01 on a local TCP port: answers every 13-byte command with
    /// a position frame and records the commands it received.
    fn fake_md01() -> (String, Arc<Mutex<Vec<Vec<u8>>>>) {
        fake_md01_with(false)
    }

    /// Like `fake_md01`, but if `moves_after_restart` is set, the rotor
    /// "works" once a restart command has been received: it then reports the
    /// most recently commanded position.
    fn fake_md01_with(moves_after_restart: bool) -> (String, Arc<Mutex<Vec<Vec<u8>>>>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let received = Arc::new(Mutex::new(Vec::new()));
        let log = received.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let log = log.clone();
                std::thread::spawn(move || {
                    let mut command = [0u8; 13];
                    while stream.read_exact(&mut command).is_ok() {
                        let mut position = [0x58, 3, 6, 0, 0, 0, 3, 6, 0, 0, 0, 0x20];
                        {
                            let mut log = log.lock().unwrap();
                            log.push(command.to_vec());
                            let restarted = log.iter().any(|c| is_restart(c));
                            // Last set-direction command: ASCII digits of
                            // (angle + 360) × 100 for az and el.
                            let last_set = log.iter().rev().find(|c| c[11] == 0x5F);
                            if let (true, true, Some(set)) =
                                (moves_after_restart, restarted, last_set)
                            {
                                for i in 1..=10 {
                                    position[i] = set[i] - 0x30;
                                }
                            }
                        }
                        if stream.write_all(&position).is_err() {
                            return;
                        }
                    }
                });
            }
        });
        (address, received)
    }

    #[tokio::test]
    async fn restart_request_sends_restart_command_and_reports_ack() {
        let (address, received) = fake_md01();
        let tracker = TelescopeTracker::new(
            address,
            Location {
                longitude: 0.0,
                latitude: 0.0,
            },
            0.0,
            std::f64::consts::PI,
            TleCacheHandle::new(),
        );
        let receiver = tracker.request_restart().unwrap();
        // A second request while one is pending is refused.
        assert!(matches!(
            tracker.request_restart(),
            Err(TelescopeError::TelescopeBusy)
        ));
        let result = tokio::time::timeout(Duration::from_secs(10), receiver)
            .await
            .expect("restart should be sent within a few tracker cycles")
            .unwrap();
        assert_eq!(result, Ok(()));
        let restart = [
            0x57, 0xEF, 0xBE, 0xAD, 0xDE, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xEE, 0x20,
        ];
        assert!(received.lock().unwrap().iter().any(|c| c == &restart));
        tracker.shutdown().await;
    }

    fn is_restart(command: &[u8]) -> bool {
        command.get(11) == Some(&0xEE)
    }

    #[tokio::test]
    async fn stuck_antenna_gets_one_automatic_restart_only() {
        // The fake MD01 always reports az 0°, el 0° and never moves.
        let (address, received) = fake_md01();
        let mut tracker = TelescopeTracker::new(
            address,
            Location {
                longitude: 0.0,
                latitude: 0.0,
            },
            0.0,
            std::f64::consts::PI,
            TleCacheHandle::new(),
        );
        tracker
            .set_target(
                TelescopeTarget::Horizontal {
                    azimuth: 90.0_f64.to_radians(),
                    elevation: 45.0_f64.to_radians(),
                },
                0.0,
                0.0,
            )
            .unwrap();
        // Long enough for stall, restart, reconnect and a second stall.
        let enough = STALL_TIMEOUT * 3 + RESTART_SETTLE + Duration::from_secs(4);
        tokio::time::sleep(enough).await;
        let restarts = received
            .lock()
            .unwrap()
            .iter()
            .filter(|c| is_restart(c))
            .count();
        assert_eq!(restarts, 1);
        assert!(matches!(
            tracker.info().unwrap().most_recent_error,
            Some(TelescopeError::TelescopeNotMoving { .. })
        ));
        tracker.shutdown().await;
    }

    #[tokio::test]
    async fn automatic_restart_brings_a_stuck_antenna_back_to_tracking() {
        let (address, received) = fake_md01_with(true);
        let mut tracker = TelescopeTracker::new(
            address,
            Location {
                longitude: 0.0,
                latitude: 0.0,
            },
            0.0,
            std::f64::consts::PI,
            TleCacheHandle::new(),
        );
        tracker
            .set_target(
                TelescopeTarget::Horizontal {
                    azimuth: 90.0_f64.to_radians(),
                    elevation: 45.0_f64.to_radians(),
                },
                0.0,
                0.0,
            )
            .unwrap();
        let enough = STALL_TIMEOUT * 2 + RESTART_SETTLE + Duration::from_secs(4);
        tokio::time::sleep(enough).await;
        let info = tracker.info().unwrap();
        assert_eq!(info.status, TelescopeStatus::Tracking);
        assert_eq!(info.most_recent_error, None);
        let restarts = received
            .lock()
            .unwrap()
            .iter()
            .filter(|c| is_restart(c))
            .count();
        assert_eq!(restarts, 1);
        tracker.shutdown().await;
    }

    #[test]
    fn stall_reported_when_antenna_does_not_move_towards_far_target() {
        let start = std::time::Instant::now();
        let mut stall = StallDetector::default();
        let timeout = STALL_TIMEOUT.as_secs();
        for s in 0..timeout {
            stall.update(deg(100.0, 30.0), true, start + Duration::from_secs(s));
            assert!(!stall.is_stalled(), "too early at {s} s");
        }
        stall.update(deg(100.0, 30.0), true, start + STALL_TIMEOUT);
        assert!(stall.is_stalled());
        // Movement clears it.
        stall.update(
            deg(101.0, 30.0),
            true,
            start + STALL_TIMEOUT + Duration::from_secs(1),
        );
        assert!(!stall.is_stalled());
    }

    #[test]
    fn no_stall_while_slewing_or_tracking() {
        let start = std::time::Instant::now();
        let mut stall = StallDetector::default();
        // Slewing: position changes every second.
        for s in 0..30 {
            stall.update(
                deg(100.0 + s as f64, 30.0),
                true,
                start + Duration::from_secs(s),
            );
            assert!(!stall.is_stalled());
        }
        // Tracking: close to target, standing still is fine.
        for s in 30..60 {
            stall.update(deg(130.0, 30.0), false, start + Duration::from_secs(s));
            assert!(!stall.is_stalled());
        }
    }

    // Past behaviour: route handlers racing teardown would call into the
    // tracker after shutdown set state.quit=true and the four entry points
    // would panic the tokio worker (seen in /var/log/syslog.3.gz on 2026-05-08).
    // Now each method must return TelescopeNotConnected instead.
    #[tokio::test]
    async fn methods_after_shutdown_return_not_connected_instead_of_panicking() {
        let mut tracker = TelescopeTracker::new(
            // Reserved port 0 / unroutable address — the background task's
            // connect attempts will fail, but that's fine for this test:
            // we only exercise the trait-method paths.
            "127.0.0.1:1".to_string(),
            Location {
                longitude: 0.0,
                latitude: 0.0,
            },
            0.0,
            std::f64::consts::PI,
            TleCacheHandle::new(),
        );
        tracker.shutdown().await;

        assert!(matches!(
            tracker.info(),
            Err(TelescopeError::TelescopeNotConnected)
        ));
        assert!(matches!(
            tracker.stop(),
            Err(TelescopeError::TelescopeNotConnected)
        ));
        assert!(matches!(
            tracker.set_target(TelescopeTarget::Sun, 0.0, 0.0),
            Err(TelescopeError::TelescopeNotConnected)
        ));
        assert!(matches!(
            tracker.request_restart(),
            Err(TelescopeError::TelescopeNotConnected)
        ));
    }
}
