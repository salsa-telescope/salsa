//! A `Telescope` test double for states the real backends cannot be driven
//! into.
//!
//! `FakeTelescope` is a simulator, not a stub: its `set_target` stops any
//! integration, it never autonomously slews, and it never raises
//! `TelescopeIOError`. Tests that need "the antenna wandered off target
//! mid-integration" or "the rotator controller dropped while the receiver
//! kept running" have to supply the `TelescopeInfo` directly, which is what
//! this module is for.
//!
//! Shared rather than per-test because the interesting part of any such test
//! is two or three fields of `TelescopeInfo`; the other eighteen are noise
//! that would otherwise have to be restated — and re-edited — in every test
//! module that has one.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use chrono::Utc;
use tokio_util::sync::CancellationToken;

use crate::coords::{Direction, Location};
use crate::models::telescope::Telescope;
use crate::models::telescope_types::{
    CalibrationResult, IqBlock, ObservedSpectra, ReceiverConfiguration, ReceiverError,
    TelescopeError, TelescopeInfo, TelescopeStatus, TelescopeTarget,
};

/// A `TelescopeInfo` with every field at a neutral default: tracking, no
/// error, not measuring, at the origin with no elevation limits. Tests
/// override the two or three fields they are actually about.
pub fn mock_info() -> TelescopeInfo {
    TelescopeInfo {
        id: "mock".to_string(),
        status: TelescopeStatus::Tracking,
        commanded_horizontal: None,
        current_horizontal: None,
        current_target: None,
        most_recent_error: None,
        measurement_in_progress: false,
        latest_observation: None,
        stow_position: None,
        service_position: None,
        az_offset_rad: 0.0,
        el_offset_rad: 0.0,
        location: Location {
            longitude: 0.0,
            latitude: 0.0,
        },
        min_elevation_rad: 0.0,
        max_elevation_rad: std::f64::consts::PI,
        webcam_crop: None,
        receiver_connected: None,
        controller_connected: None,
        wind_warning_ms: None,
        default_ref_freq_mhz: 1417.9,
        default_gain_db: 60.0,
        receiver_configuration: ReceiverConfiguration::default(),
    }
}

/// A single-channel spectrum reporting `observation_time` of integration.
pub fn observed_for(observation_time: std::time::Duration) -> ObservedSpectra {
    ObservedSpectra {
        frequencies: vec![0.0],
        spectra: vec![0.0],
        observation_time,
        start: Utc::now(),
    }
}

/// Reports whatever its closure returns. The closure is handed the number of
/// `get_info` calls served so far (0 on the first), so a test can either
/// ignore it and return a canned result or use it to evolve the reported
/// state poll by poll.
///
/// Every other trait method is either a no-op or returns a benign value —
/// none of them panic, so adding a method to `Telescope` does not break
/// tests that never call it.
pub struct MockTelescope {
    info: Box<dyn Fn(u64) -> Result<TelescopeInfo, TelescopeError> + Send + Sync>,
    polls: AtomicU64,
    /// Polls served at the moment `stop_integration` was first called, or 0
    /// if it was never called.
    polls_at_stop: AtomicU64,
}

impl MockTelescope {
    /// Reports whatever `info` computes from the poll count.
    pub fn new(
        info: impl Fn(u64) -> Result<TelescopeInfo, TelescopeError> + Send + Sync + 'static,
    ) -> Arc<Self> {
        Arc::new(MockTelescope {
            info: Box::new(info),
            polls: AtomicU64::new(0),
            polls_at_stop: AtomicU64::new(0),
        })
    }

    /// Reports the same result on every poll.
    pub fn returning(info: Result<TelescopeInfo, TelescopeError>) -> Arc<Self> {
        Self::new(move |_| info.clone())
    }

    /// Whether `stop_integration` has been called.
    pub fn stopped(&self) -> bool {
        self.polls_at_stop.load(Ordering::SeqCst) > 0
    }

    /// Polls served when `stop_integration` was called, or 0 if it never was.
    pub fn polls_at_stop(&self) -> u64 {
        self.polls_at_stop.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl Telescope for MockTelescope {
    async fn get_info(&self) -> Result<TelescopeInfo, TelescopeError> {
        (self.info)(self.polls.fetch_add(1, Ordering::SeqCst))
    }

    async fn stop_integration(&self) -> Option<ObservedSpectra> {
        // `max(1)` so a stop on the very first poll is still distinguishable
        // from never having been called.
        self.polls_at_stop
            .store(self.polls.load(Ordering::SeqCst).max(1), Ordering::SeqCst);
        Some(observed_for(std::time::Duration::from_secs(1)))
    }

    async fn set_target(
        &self,
        target: TelescopeTarget,
        _az_offset_rad: f64,
        _el_offset_rad: f64,
    ) -> Result<TelescopeTarget, TelescopeError> {
        Ok(target)
    }

    async fn stop(&self) -> Result<(), TelescopeError> {
        Ok(())
    }

    async fn calibrate(
        &self,
        az_offset_rad: f64,
        el_offset_rad: f64,
    ) -> Result<CalibrationResult, TelescopeError> {
        // Reports the offsets as having been applied to a zeroed position,
        // which is the shape callers expect without pretending to model the
        // rotator's stored coordinates.
        Ok(CalibrationResult {
            previous: direction(0.0, 0.0),
            adjusted: direction(-az_offset_rad, -el_offset_rad),
        })
    }

    async fn set_receiver_configuration(
        &self,
        receiver_configuration: ReceiverConfiguration,
    ) -> Result<ReceiverConfiguration, ReceiverError> {
        Ok(receiver_configuration)
    }

    async fn clear_measurements(&self) {}

    async fn interferometry_capable(&self) -> bool {
        false
    }

    async fn current_integration_token(&self) -> Option<CancellationToken> {
        None
    }

    async fn shutdown(&self) {}

    async fn start_iq_stream(
        &self,
        _config: ReceiverConfiguration,
    ) -> Result<tokio::sync::mpsc::Receiver<IqBlock>, ReceiverError> {
        Err(ReceiverError::IntegrationAlreadyRunning)
    }
}

/// Convenience for the common "antenna is here, pointing there" shape.
pub fn direction(azimuth: f64, elevation: f64) -> Direction {
    Direction { azimuth, elevation }
}
