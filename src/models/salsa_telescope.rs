use crate::coords::{Direction, Location};
use crate::models::telescope::Telescope;
use crate::models::telescope_types::{
    CalibrationResult, IQ_BLOCK_SIZE, IqBlock, Measurement, ObservationMode, ObservedSpectra,
    ReceiverConfiguration, ReceiverError, TelescopeError, TelescopeInfo, TelescopeTarget,
    sample_bits,
};
use crate::telescope_tracker::TelescopeTracker;
use crate::tle_cache::TleCacheHandle;
use async_trait::async_trait;
use chrono::Utc;
use std::iter::zip;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use std::time::Duration;

use rustfft::{FftPlanner, num_complex::Complex};
use uhd::{self, StreamCommand, StreamCommandType, StreamTime, TuneRequest, Usrp};

pub const TELESCOPE_UPDATE_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum IntegrationKind {
    /// `measure()` task — writes single-dish spectra into `Inner::measurements`.
    Spectrum,
    /// `measure_iq()` task — streams raw IQ blocks; does not touch `measurements`.
    Iq,
}

pub struct ActiveIntegration {
    cancellation_token: CancellationToken,
    /// `None` once the task has been awaited. Since `measure()` ends by itself
    /// on reaching its requested duration, the periodic reaper in
    /// `Inner::update` collects the handle to surface any error it died with —
    /// but the `ActiveIntegration` itself must outlive that, because it is what
    /// tells `stop_integration` there is a spectrum to hand back.
    measurement_task: Option<tokio::task::JoinHandle<Result<(), TelescopeError>>>,
    kind: IntegrationKind,
}

/// One open UHD session per receiver, held across whole measurement tasks.
///
/// `stop_integration` marks the receiver idle and lifts the `ActiveIntegration`
/// out of `Inner` *before* it awaits the measurement task, so for the 0.8–2.2 s
/// that await lasts the telescope reports a free receiver while the old task
/// still owns an open `Usrp`. A start arriving in that window — a user pressing
/// Observe one second after End, vale 2026-08-28 — opened a second session on
/// the same N210. The device serves one host at a time: the first session's
/// control channel then timed out on the way out (`fifo ctrl timed out looking
/// for acks`, thrown from `~usrp2_fifo_ctrl_impl`) and the process died with
/// SIGSEGV inside UHD's teardown, taking the new integration with it.
///
/// Taking this before `Usrp::open` and holding it until the device has been
/// dropped makes the second task wait for the first to let go instead. It is a
/// `std::sync::Mutex` on purpose: only `spawn_blocking` tasks touch it, and it
/// has to be held across the blocking FFI work, which an async mutex must not
/// be.
type DeviceLock = Arc<std::sync::Mutex<()>>;

/// How long a starting integration waits for the previous one to release the
/// receiver before giving up. Long enough to cover any legitimate handover —
/// the old task can only exit at a cycle boundary, and the widest bandwidths
/// make those the slowest — but bounded, so a wedged session surfaces as an
/// error on the page instead of a start that never happens.
const DEVICE_HANDOVER_TIMEOUT: Duration = Duration::from_secs(30);

/// Wait for exclusive use of the receiver at `address`. See [`DeviceLock`].
fn acquire_device<'a>(
    lock: &'a DeviceLock,
    address: &str,
) -> Result<std::sync::MutexGuard<'a, ()>, TelescopeError> {
    acquire_device_within(lock, address, DEVICE_HANDOVER_TIMEOUT)
}

/// [`acquire_device`] with the wait spelled out, so a test does not have to sit
/// through the production timeout to see what happens when it expires.
fn acquire_device_within<'a>(
    lock: &'a DeviceLock,
    address: &str,
    timeout: Duration,
) -> Result<std::sync::MutexGuard<'a, ()>, TelescopeError> {
    let started = std::time::Instant::now();
    let mut waited = false;
    loop {
        match lock.try_lock() {
            Ok(guard) => {
                if waited {
                    info!(
                        "Receiver at {address} released by the previous integration after {:?}",
                        started.elapsed()
                    );
                }
                return Ok(guard);
            }
            // A measurement task that panicked with the device open poisons the
            // lock. The `Usrp` was dropped as the panic unwound, so the receiver
            // is free regardless of what the flag says.
            Err(std::sync::TryLockError::Poisoned(poisoned)) => return Ok(poisoned.into_inner()),
            Err(std::sync::TryLockError::WouldBlock) => {
                if !waited {
                    info!(
                        "Waiting for the previous integration to release the receiver at {address}"
                    );
                    waited = true;
                }
                if started.elapsed() >= timeout {
                    return Err(TelescopeError::ReceiverFailed(format!(
                        "the previous integration still holds the receiver at {address} after {:?}",
                        timeout
                    )));
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

/// Guards UHD's process-wide registry of open devices.
///
/// UHD's C API, which the `uhd` crate wraps, keeps every open device in one
/// global `std::map` (`get_usrp_ptrs()` in `host/lib/usrp/usrp_c.cpp`).
/// `uhd_usrp_make` inserts into it and `uhd_usrp_free` erases from it, but
/// under two *different* mutexes, and every device call (`USRP(h)`) looks the
/// device up with no lock at all. One telescope opening its receiver while
/// another closes its own can therefore corrupt the map or have it read
/// mid-update. That is what killed the process on 2026-10-02 (brage opening
/// as vale closed) and 2026-10-09 (vale opening as brage closed): both cores
/// show SIGSEGV in `uhd_usrp_set_rx_freq`, the first device call of the next
/// integration cycle. UHD 4.6 and master both have this. [`DeviceLock`]
/// cannot help, as it is per receiver and this race is between receivers.
///
/// Opening and freeing take this exclusively; every other device call takes it
/// shared, so running telescopes do not wait on each other. Streamer calls
/// (`receive_simple`, stream commands) go through the streamer handle and never
/// touch the map, so the sample loop runs unlocked and an open never waits out
/// another telescope's integration cycle. It does wait out a whole open or
/// free (about a second on an N210), which can delay another telescope's next
/// cycle by that much; the integrated time is counted in cycles, so no data
/// is misreported.
static UHD_REGISTRY: std::sync::RwLock<()> = std::sync::RwLock::new(());

/// A UHD device that touches UHD's global registry only under
/// [`UHD_REGISTRY`]: open and free exclusively, every device call through
/// [`UhdDevice::with`] shared. Generic so the locking can be tested without
/// hardware; production always uses `Usrp`.
struct UhdDevice<D = Usrp> {
    device: std::mem::ManuallyDrop<D>,
    address: String,
}

impl UhdDevice<Usrp> {
    fn open(address: &str) -> Result<Self, uhd::Error> {
        Self::open_with(address, || Usrp::open(&format!("addr={address}")))
    }
}

impl<D> UhdDevice<D> {
    fn open_with<E>(address: &str, open: impl FnOnce() -> Result<D, E>) -> Result<Self, E> {
        // A panic while holding the lock leaves the registry no worse than UHD
        // left it, so a poisoned lock is still usable.
        let _registry = UHD_REGISTRY
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Ok(UhdDevice {
            device: std::mem::ManuallyDrop::new(open()?),
            address: address.to_string(),
        })
    }

    /// Run one device call. The result may borrow the device (a receive
    /// streamer does) and outlive the lock, which is fine because streamer
    /// calls do not touch the registry.
    fn with<'a, R>(&'a mut self, call: impl FnOnce(&'a mut D) -> R) -> R {
        let _registry = UHD_REGISTRY
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        call(&mut self.device)
    }
}

impl<D> Drop for UhdDevice<D> {
    fn drop(&mut self) {
        let started = std::time::Instant::now();
        {
            let _registry = UHD_REGISTRY
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            // SAFETY: dropped exactly once, here, and never touched again.
            unsafe { std::mem::ManuallyDrop::drop(&mut self.device) };
        }
        info!(
            "Released receiver at {} after {} ms",
            self.address,
            started.elapsed().as_millis()
        );
    }
}

struct Inner {
    name: String,
    receiver_address: String,
    gpsdo_enabled: bool,
    controller: TelescopeTracker,
    receiver_configuration: ReceiverConfiguration,
    measurements: Arc<Mutex<Vec<Measurement>>>,
    active_integration: Option<ActiveIntegration>,
    last_receiver_error: Option<TelescopeError>,
    stow_position: Option<Direction>,
    service_position: Option<Direction>,
    location: Location,
    min_elevation_rad: f64,
    max_elevation_rad: f64,
    webcam_crop: Option<[f64; 4]>,
    default_ref_freq_hz: f64,
    default_gain_db: f64,
    tsys_k: f64,
    wind_warning_ms: Option<f64>,
    receiver_connected: Arc<tokio::sync::Mutex<bool>>,
    controller_connected: bool,
    device_lock: DeviceLock,
}

pub struct SalsaTelescope {
    inner: Arc<Mutex<Inner>>,
    background_tasks: Mutex<Option<Vec<tokio::task::JoinHandle<()>>>>,
}

#[allow(clippy::too_many_arguments)]
pub fn create(
    name: String,
    controller_address: String,
    receiver_address: String,
    gpsdo_enabled: bool,
    stow_position: Option<Direction>,
    service_position: Option<Direction>,
    location: Location,
    min_elevation_rad: f64,
    max_elevation_rad: f64,
    webcam_crop: Option<[f64; 4]>,
    default_ref_freq_hz: f64,
    default_gain_db: f64,
    tsys_k: f64,
    wind_warning_ms: Option<f64>,
    tle_cache: TleCacheHandle,
) -> SalsaTelescope {
    let receiver_connected = Arc::new(tokio::sync::Mutex::new(false));
    let ping_connected = receiver_connected.clone();
    let ping_address = receiver_address.clone();

    let inner = Arc::new(Mutex::new(Inner {
        name,
        receiver_address,
        gpsdo_enabled,
        controller: TelescopeTracker::new(
            controller_address,
            location,
            min_elevation_rad,
            max_elevation_rad,
            tle_cache.clone(),
        ),
        receiver_configuration: ReceiverConfiguration {
            integrate: false,
            ref_freq_hz: default_ref_freq_hz,
            gain_db: default_gain_db,
            ..Default::default()
        },
        measurements: Arc::new(Mutex::new(Vec::new())),
        active_integration: None,
        last_receiver_error: None,
        stow_position,
        service_position,
        location,
        min_elevation_rad,
        max_elevation_rad,
        webcam_crop,
        default_ref_freq_hz,
        default_gain_db,
        tsys_k,
        wind_warning_ms,
        receiver_connected,
        controller_connected: false,
        device_lock: Arc::new(std::sync::Mutex::new(())),
    }));

    let task_inner = inner.clone();
    let update_task = tokio::spawn(async move {
        loop {
            {
                let mut inner = task_inner.lock().await;
                if let Err(error) = inner.update(TELESCOPE_UPDATE_INTERVAL).await {
                    error!("Failed to update telescope: {}", error);
                }
            }
            tokio::time::sleep(TELESCOPE_UPDATE_INTERVAL).await;
        }
    });

    let ping_task = tokio::spawn(async move {
        let mut prev_reachable = false;
        loop {
            let addr = ping_address.clone();
            let reachable =
                tokio::task::spawn_blocking(move || {
                    match std::process::Command::new("/usr/bin/ping")
                        .args(["-c", "1", "-W", "1", &addr])
                        .output()
                    {
                        Ok(o) => o.status.success(),
                        Err(err) => {
                            tracing::warn!("Failed to run ping for {addr}: {err}");
                            false
                        }
                    }
                })
                .await
                .unwrap_or(false);
            if reachable != prev_reachable {
                if reachable {
                    info!("Receiver at {} is reachable", ping_address);
                } else {
                    warn!("Receiver at {} is no longer reachable", ping_address);
                }
                prev_reachable = reachable;
            }
            *ping_connected.lock().await = reachable;
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    });

    SalsaTelescope {
        inner,
        background_tasks: Mutex::new(Some(vec![update_task, ping_task])),
    }
}

#[async_trait]
impl Telescope for SalsaTelescope {
    async fn set_target(
        &self,
        target: TelescopeTarget,
        az_offset_rad: f64,
        el_offset_rad: f64,
    ) -> Result<TelescopeTarget, TelescopeError> {
        let mut inner = self.inner.lock().await;
        let result = inner
            .controller
            .set_target(target, az_offset_rad, el_offset_rad);
        // The only record that a dish was commanded somewhere. `FakeTelescope`
        // has logged this since it was written, so on a development machine the
        // line appears to exist — but the real path runs through
        // `TelescopeTracker`, which logs nothing, and neither did this. A
        // production journal held no trace of what was pointed where.
        //
        // Fires once per user action (a form submission), not once per tracking
        // tick: the 1 Hz observe-page traffic is `GET /telescope/{id}/state`,
        // and the continuous position updates happen inside the tracker's own
        // loop. Three telescopes booked all day is tens of lines, in the same
        // range as the "Starting integration" line just below.
        match &result {
            Ok(_) => info!(
                "Setting target for telescope {} to {} (offsets az={:.2}°, el={:.2}°)",
                inner.name,
                target,
                az_offset_rad.to_degrees(),
                el_offset_rad.to_degrees()
            ),
            Err(err) => info!(
                "Refusing to set target for telescope {} to {}: {}",
                inner.name, target, err
            ),
        }
        result
    }

    async fn stop(&self) -> Result<(), TelescopeError> {
        let mut inner = self.inner.lock().await;
        inner.controller.stop()
    }

    async fn calibrate(
        &self,
        az_offset_rad: f64,
        el_offset_rad: f64,
    ) -> Result<CalibrationResult, TelescopeError> {
        // Request the calibration while holding the lock, but await the
        // outcome outside it: the tracker task executes on its next cycle
        // and other trait methods must not block on it.
        let receiver = {
            let inner = self.inner.lock().await;
            inner
                .controller
                .request_calibration(az_offset_rad, el_offset_rad)?
        };
        match tokio::time::timeout(Duration::from_secs(10), receiver).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(TelescopeError::TelescopeIOError(
                "Calibration task ended without reporting a result".to_string(),
            )),
            Err(_) => Err(TelescopeError::TelescopeIOError(
                "Calibration timed out".to_string(),
            )),
        }
    }

    async fn set_receiver_configuration(
        &self,
        receiver_configuration: ReceiverConfiguration,
    ) -> Result<ReceiverConfiguration, ReceiverError> {
        let mut inner = self.inner.lock().await;
        if receiver_configuration.integrate && !inner.receiver_configuration.integrate {
            if inner.active_integration.is_some() {
                return Err(ReceiverError::IntegrationAlreadyRunning);
            }

            info!("Starting integration on {}", inner.name);
            // Keep the whole configuration, not just the flag. The rest of it
            // used to go straight to `measure()` and be forgotten, so nothing
            // downstream — `get_info`, and through it the saved observation —
            // could report what gain or mode a spectrum was actually taken at.
            inner.receiver_configuration = receiver_configuration;
            inner.last_receiver_error = None;
            inner.measurements.lock().await.clear();
            let cancellation_token = CancellationToken::new();
            let measurement_task = {
                let address = inner.receiver_address.clone();
                let measurements = inner.measurements.clone();
                let cancellation_token = cancellation_token.clone();
                let tsys_k = inner.tsys_k;
                let device_lock = inner.device_lock.clone();
                tokio::task::spawn_blocking(move || {
                    measure(
                        address,
                        measurements,
                        cancellation_token,
                        receiver_configuration,
                        tsys_k,
                        device_lock,
                    )
                })
            };
            inner.active_integration = Some(ActiveIntegration {
                cancellation_token,
                measurement_task: Some(measurement_task),
                kind: IntegrationKind::Spectrum,
            });
        } else if !receiver_configuration.integrate && inner.receiver_configuration.integrate {
            info!("Stopping integration on {}", inner.name);
            inner.receiver_configuration.integrate = false;
            let result = inner.receiver_configuration;
            // Take the task out so we can await it without holding the inner lock,
            // which would deadlock with the task trying to lock measurements. This
            // matches stop_integration() and guarantees a subsequent start_iq_stream
            // won't see a stale active_integration.
            let active = inner.active_integration.take();
            drop(inner);
            if let Some(ai) = active {
                ai.cancellation_token.cancel();
                // Already `None` when the reaper in `update` collected it.
                if let Some(task) = ai.measurement_task {
                    match task.await {
                        Ok(Ok(())) => {}
                        Ok(Err(err)) => error!("Measurement task failed before stop: {err}"),
                        Err(join_err) => {
                            error!("Error waiting for measurement task to finish: {join_err}")
                        }
                    }
                }
            }
            return Ok(result);
        }
        Ok(inner.receiver_configuration)
    }

    async fn stop_integration(&self) -> Option<ObservedSpectra> {
        let active_integration = {
            let mut inner = self.inner.lock().await;
            if !inner.receiver_configuration.integrate {
                return None;
            }
            info!("Stopping integration on {}", inner.name);
            inner.receiver_configuration.integrate = false;
            inner.active_integration.take()
        };
        // Lock is dropped — safe to await the task without risk of deadlock.
        let kind = active_integration.as_ref().map(|ai| ai.kind);
        if let Some(ai) = active_integration {
            ai.cancellation_token.cancel();
            // Already `None` when the reaper in `update` collected it — which is
            // the normal case now that `measure()` ends on its own.
            if let Some(task) = ai.measurement_task {
                match task.await {
                    Ok(Ok(())) => {}
                    Ok(Err(err)) => error!("Measurement task failed before stop: {err}"),
                    Err(join_err) => {
                        error!("Error waiting for measurement task to finish: {join_err}")
                    }
                }
            }
        }
        // IQ streams do not produce ObservedSpectra. Returning the last entry of
        // `measurements` would surface stale single-dish data from earlier in the
        // process lifetime and mis-file it under the current user/target.
        if kind != Some(IntegrationKind::Spectrum) {
            warn!(
                "Stopped an integration that yielded no spectrum to save: kind was {kind:?} \
                 rather than Spectrum (None means the integration had already been claimed)"
            );
            return None;
        }
        let inner = self.inner.lock().await;
        let measurements = inner.measurements.lock().await;
        let spectra = measurements.last().map(|m| ObservedSpectra {
            frequencies: m.freqs.clone(),
            spectra: m.amps.clone(),
            observation_time: m.duration,
            start: m.start,
        });
        if spectra.is_none() {
            warn!(
                "Stopped an integration on {} with an empty measurement buffer, so there is \
                 nothing to save",
                inner.name
            );
        }
        spectra
    }

    async fn clear_measurements(&self) {
        let inner = self.inner.lock().await;
        inner.measurements.lock().await.clear();
    }

    async fn interferometry_capable(&self) -> bool {
        self.inner.lock().await.gpsdo_enabled
    }

    async fn current_integration_token(&self) -> Option<CancellationToken> {
        self.inner
            .lock()
            .await
            .active_integration
            .as_ref()
            .map(|ai| ai.cancellation_token.clone())
    }

    async fn get_info(&self) -> Result<TelescopeInfo, TelescopeError> {
        let inner = self.inner.lock().await;
        let receiver_connected = *inner.receiver_connected.lock().await;
        let controller_info = inner.controller.info()?;

        let latest_observation = {
            let measurements = inner.measurements.lock().await;
            match measurements.last() {
                None => None,
                Some(measurement) => {
                    let measurement = measurement.clone();
                    let latest_observation = ObservedSpectra {
                        frequencies: measurement.freqs,
                        spectra: measurement.amps,
                        observation_time: measurement.duration,
                        start: measurement.start,
                    };
                    Some(latest_observation)
                }
            }
        };

        Ok(TelescopeInfo {
            id: inner.name.clone(),
            status: controller_info.status,
            current_horizontal: controller_info.current_horizontal,
            commanded_horizontal: controller_info.commanded_horizontal,
            current_target: controller_info.target,
            most_recent_error: inner
                .last_receiver_error
                .clone()
                .or(controller_info.most_recent_error),
            measurement_in_progress: inner
                .active_integration
                .as_ref()
                .is_some_and(|ai| ai.kind == IntegrationKind::Spectrum),
            latest_observation,
            stow_position: inner.stow_position,
            service_position: inner.service_position,
            az_offset_rad: controller_info.az_offset_rad,
            el_offset_rad: controller_info.el_offset_rad,
            location: inner.location,
            min_elevation_rad: inner.min_elevation_rad,
            max_elevation_rad: inner.max_elevation_rad,
            webcam_crop: inner.webcam_crop,
            receiver_connected: Some(receiver_connected),
            controller_connected: Some(inner.controller_connected),
            wind_warning_ms: inner.wind_warning_ms,
            default_ref_freq_mhz: inner.default_ref_freq_hz / 1e6,
            default_gain_db: inner.default_gain_db,
            receiver_configuration: inner.receiver_configuration,
        })
    }
    /// Shut down in four steps, each of which can block on hardware.
    ///
    /// Every step announces itself *before* it runs, not just after. A step
    /// that never returns cannot report its own duration, so the diagnostic
    /// value is in the last line that has no matching completion — that names
    /// the one that hung. At `info` deliberately: production runs
    /// `RUST_LOG=info`, so the `debug!` that used to be here was never visible
    /// on the server, which is part of why an unbounded shutdown went
    /// unexplained for so long. Shutdown happens once per restart, so the
    /// handful of extra lines costs nothing.
    async fn shutdown(&self) {
        let started = std::time::Instant::now();

        // Bound the guard to this statement so the lock is definitely released
        // before the loop below awaits.
        info!("telescope shutdown: 1/4 taking background-task lock");
        let tasks = self.background_tasks.lock().await.take();

        if let Some(tasks) = tasks {
            for (index, task) in tasks.into_iter().enumerate() {
                // `abort()` does not interrupt a `spawn_blocking` closure, so
                // this await can outlast the abort by however long the inner
                // blocking work takes to return.
                info!("telescope shutdown: 2/4 stopping background task {index}");
                task.abort();
                let _ = task.await;
            }
        }
        info!(
            "telescope shutdown: background tasks stopped after {:?}",
            started.elapsed()
        );

        info!("telescope shutdown: 3/4 taking inner lock");
        let inner = self.inner.lock().await;

        info!(
            "telescope shutdown: 4/4 closing controller connection for {}",
            inner.name
        );
        inner.controller.shutdown().await;

        info!(
            "telescope shutdown: {} closed after {:?}",
            inner.name,
            started.elapsed()
        );
    }

    async fn start_iq_stream(
        &self,
        config: ReceiverConfiguration,
    ) -> Result<tokio::sync::mpsc::Receiver<IqBlock>, ReceiverError> {
        let mut inner = self.inner.lock().await;
        if inner.active_integration.is_some() {
            return Err(ReceiverError::IntegrationAlreadyRunning);
        }
        let (tx, rx) = tokio::sync::mpsc::channel(8);
        let cancellation_token = CancellationToken::new();
        let measurement_task = {
            let address = inner.receiver_address.clone();
            let gpsdo_enabled = inner.gpsdo_enabled;
            let token = cancellation_token.clone();
            let device_lock = inner.device_lock.clone();
            tokio::task::spawn_blocking(move || {
                measure_iq(address, gpsdo_enabled, token, config, tx, device_lock)
            })
        };
        // Keep the whole configuration, not just the flag, so `get_info`
        // reports the bandwidth this stream is using. The network-link budget
        // (see `bandwidths_in_use_hz`) counts it from there.
        inner.receiver_configuration = ReceiverConfiguration {
            integrate: true,
            ..config
        };
        inner.active_integration = Some(ActiveIntegration {
            cancellation_token,
            measurement_task: Some(measurement_task),
            kind: IntegrationKind::Iq,
        });
        Ok(rx)
    }
}

impl Inner {
    async fn update(&mut self, _delta_time: Duration) -> Result<(), TelescopeError> {
        // Collect a measurement task that has finished on its own so an error it
        // died with reaches the page. Only the handle is taken: the
        // `ActiveIntegration` has to stay until the integration is actually
        // stopped, because `stop_integration` uses it to recognise the run and
        // return its spectrum. Taking the whole thing here left `integrate` true
        // with nothing attached, and the finished spectrum was then discarded —
        // which is what silently lost one-second integrations, since those are
        // over long before anything stops them.
        let finished_task =
            self.active_integration
                .as_mut()
                .and_then(|active| match &active.measurement_task {
                    Some(task) if task.is_finished() => active.measurement_task.take(),
                    _ => None,
                });
        if let Some(task) = finished_task {
            match task.await {
                Ok(Ok(())) => {}
                Ok(Err(err)) => {
                    error!("Measurement task failed: {}", err);
                    self.last_receiver_error = Some(err);
                }
                Err(join_err) => {
                    error!("Measurement task panicked: {}", join_err);
                    self.last_receiver_error =
                        Some(TelescopeError::ReceiverFailed(join_err.to_string()));
                }
            }
        }
        let connected = !matches!(
            self.controller.info().map(|i| i.most_recent_error),
            Ok(Some(
                TelescopeError::TelescopeIOError(_) | TelescopeError::TelescopeNotConnected
            )) | Err(TelescopeError::TelescopeIOError(_) | TelescopeError::TelescopeNotConnected)
        );
        if connected != self.controller_connected {
            if connected {
                info!("Controller for {} is now connected", self.name);
            } else {
                warn!("Controller for {} is no longer connected", self.name);
            }
            self.controller_connected = connected;
        }
        Ok(())
    }
}

// Reading the documentation of the telescope, this should be the correct way to interpret the bytes
// This would match how rot2prog_angle_to_bytes works.
fn rot2prog_bytes_to_int_documented(bytes: &[u8]) -> u32 {
    bytes
        .iter()
        .rev()
        .enumerate()
        .map(|(pos, &digit)| (digit as u32 - 0x30) * 10_u32.pow(pos as u32))
        .sum()
}

#[allow(dead_code)]
fn rot2prog_bytes_to_angle_documented(bytes: &[u8]) -> f64 {
    (rot2prog_bytes_to_int_documented(bytes) as f64 / 100.0 - 360.0).to_radians()
}

/// One raw I or Q component as the receiver delivers it: `i16` for 16-bit
/// samples, `i8` for 8-bit (see `sample_bits`).
trait RawSample: Copy + Default + Send + 'static + Into<f64> {
    /// UHD's name for this format, used both on the wire and on the host so
    /// the values arrive exactly as sent.
    const FORMAT: &'static str;
    /// True at the format's limits, where the value may have been clipped.
    fn at_limit(self) -> bool;
}

impl RawSample for i16 {
    const FORMAT: &'static str = "sc16";
    fn at_limit(self) -> bool {
        self == i16::MAX || self == i16::MIN
    }
}

impl RawSample for i8 {
    const FORMAT: &'static str = "sc8";
    fn at_limit(self) -> bool {
        self == i8::MAX || self == i8::MIN
    }
}

/// One measurement cycle's raw samples, handed from the receiving thread to
/// the processing thread: the signal block and, when frequency-switched, the
/// reference block.
struct CycleSamples<T> {
    sig: Vec<Complex<T>>,
    reference: Option<Vec<Complex<T>>>,
}

/// How many received cycles may wait for processing. Processing normally keeps
/// well ahead of the receiver, so the queue only fills if it falls behind; the
/// receiver then skips whole cycles instead of waiting. At 25 MHz a cycle holds
/// ~100 MB of samples, so this also bounds memory per telescope.
const PIPELINE_DEPTH: usize = 2;

/// Widest bandwidth that still gets LO-offset tuning. Sun spectra
/// (2026-07-18) showed the MAX2112's intrinsic baseband response is flat
/// to ~7 MHz from centre but droops ~1.4 dB by 15 MHz and ~4 dB by
/// 23 MHz, so at 10 and 25 MHz no offset keeps the band flat — those
/// revert to plain centre tuning, whose narrow DC dip is far less
/// harmful there than a multi-dB tilt. (The 10 MHz dome seen in the
/// same tests is unrelated CIC droop: decimation 10 engages only one
/// compensating halfband. Present with or without the offset. The
/// 10 MHz option was therefore replaced by 12.5 MHz — decimation 8,
/// which engages both halfbands.)
const LO_OFFSET_MAX_BW_HZ: f64 = 5e6;

/// LO offset for direct-conversion tuning (see receive_block): exactly
/// one bandwidth, i.e. one output sample rate. The DDC's decimation
/// chain has a deep null there, so the LO leakage/DC artifact is
/// annihilated rather than merely attenuated — a fractional offset
/// (tried in v1.2.1) parks the artifact in the decimation filters'
/// transition band, and its alias folds back into the passband as a
/// narrow spike at (bandwidth − offset) from centre. Zero — plain
/// centre tuning — above [`LO_OFFSET_MAX_BW_HZ`].
fn lo_offset_hz(bandwidth_hz: f64) -> f64 {
    if bandwidth_hz <= LO_OFFSET_MAX_BW_HZ {
        bandwidth_hz
    } else {
        0.0
    }
}

/// DBSRX2 analog filter setting: wide open at its 80 MHz maximum. The
/// filter's anti-aliasing role is preserved (40 MHz single-sided corner
/// vs the N210 ADC's 50 MHz Nyquist), and the LO-offset band then sits
/// on the flat part of the response at every selectable bandwidth (the
/// worst case, 25 MHz, ends 27.5 MHz out — 12.5 MHz inside the corner).
const DBSRX2_BANDWIDTH_HZ: f64 = 80e6;

/// Tune to `cfreq` and fill `buffer` with that many fresh samples. Runs on the
/// receiving thread, which owns the device; the CPU work happens elsewhere
/// (see [`SpectrumProcessor`]) so the receiver is never idle waiting for it.
/// `stream_args` carries extra UHD stream arguments, such as the 8-bit
/// scaling `peak=…`; empty for 16-bit.
fn receive_block<T: RawSample>(
    usrp: &mut UhdDevice,
    cfreq: f64,
    srate: f64,
    stream_args: &str,
    buffer: &mut [Complex<T>],
) -> Result<(), TelescopeError>
where
    Complex<T>: uhd::Item,
{
    // The N210 only has one input channel 0. Tune with an LO offset: the
    // DBSRX2 is direct-conversion, so tuning the LO exactly to cfreq puts
    // its leakage/DC artifact in the centre of the band (visible as a
    // narrow dip once the DC-offset correction notches it out). With the
    // offset, the RF LO parks outside the kept passband and the DDC
    // shifts back. See issue #311; requires the wide-open analog filter
    // set at receiver setup.
    usrp.with(|u| {
        u.set_rx_frequency(
            &TuneRequest::with_frequency_lo(cfreq, lo_offset_hz(srate)),
            0,
        )
    })
    .map_err(|e| TelescopeError::ReceiverFailed(format!("set_rx_frequency: {e}")))?;

    let args = uhd::StreamArgs::<Complex<T>>::builder()
        .wire_format(T::FORMAT.to_string())
        .args(stream_args.to_string())
        .build();
    let mut receiver = usrp.with(|u| u.get_rx_stream(&args)).map_err(|e| {
        TelescopeError::ReceiverFailed(format!("get_rx_stream ({}): {e}", T::FORMAT))
    })?;

    receiver
        .send_command(&StreamCommand {
            command_type: StreamCommandType::CountAndDone(buffer.len() as u64),
            time: StreamTime::Now,
        })
        .map_err(|e| TelescopeError::ReceiverFailed(format!("stream CountAndDone command: {e}")))?;
    receiver
        .receive_simple(buffer)
        .map_err(|e| TelescopeError::ReceiverFailed(format!("receive_simple: {e}")))?;
    Ok(())
}

/// RMS of one I or Q component, in the sample's own counts.
fn component_rms<T: RawSample>(samples: &[Complex<T>]) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    let power: f64 = samples
        .iter()
        .map(|x| {
            let (re, im): (f64, f64) = (x.re.into(), x.im.into());
            re * re + im * im
        })
        .sum();
    (power / (2 * samples.len()) as f64).sqrt()
}

/// How many I and Q values in `samples` sit at the format's limits.
fn count_at_limit<T: RawSample>(samples: &[Complex<T>]) -> usize {
    samples
        .iter()
        .map(|x| x.re.at_limit() as usize + x.im.at_limit() as usize)
        .sum()
}

/// Target level for 8-bit samples, in counts RMS per component, out of ±127.
/// Coarse enough levels add quantization noise (1/12 count² against 400 for
/// the signal: 37 dB down), fine enough leave 6× headroom before clipping, so
/// a reference half a few dB louder than the signal half still fits.
const EIGHT_BIT_TARGET_RMS: f64 = 20.0;

/// Length of each level-measuring snapshot taken before an 8-bit run.
const AUTO_LEVEL_SECS: f64 = 0.1;

/// UHD's 8-bit scaling, `peak`: the fraction of 16-bit full scale that maps to
/// 8-bit full scale, so 8-bit counts are 16-bit counts / (peak · 256). Chosen
/// to put a signal of `rms16` counts (16-bit) at [`EIGHT_BIT_TARGET_RMS`].
/// UHD refuses anything below 1/256.
fn eight_bit_peak(rms16: f64) -> f64 {
    (rms16 / (256.0 * EIGHT_BIT_TARGET_RMS)).clamp(1.0 / 256.0, 1.0)
}

/// How an 8-bit run is received and scaled back.
struct EightBitLevel {
    /// UHD stream arguments setting the scaling.
    stream_args: String,
    /// 8-bit counts corresponding to ADC full scale, so spectra come out on the
    /// same absolute scale as 16-bit ones.
    full_scale: f64,
}

/// Set the 8-bit scaling from the signal itself. The level varies far too much
/// between targets for one fixed scaling: a strong GNSS satellite at 25 MHz
/// sits ~16× higher than blank sky. Takes a short 16-bit snapshot at the
/// signal frequency to measure the level, picks the scaling, then a short
/// 8-bit snapshot to measure what it actually gives. The ratio of the two is
/// the scale back to 16-bit units, measured rather than taken from UHD's
/// documentation. The reference half of a frequency-switched run uses the
/// same scaling, so any error in it cancels in the sig/ref ratio.
fn auto_level(
    usrp: &mut UhdDevice,
    address: &str,
    sfreq: f64,
    srate: f64,
) -> Result<EightBitLevel, TelescopeError> {
    let len = (AUTO_LEVEL_SECS * srate) as usize;
    let mut wide = vec![Complex::<i16>::default(); len];
    receive_block(usrp, sfreq, srate, "", &mut wide)?;
    let rms16 = component_rms(&wide);

    let peak = eight_bit_peak(rms16);
    let stream_args = format!("peak={peak}");
    let mut narrow = vec![Complex::<i8>::default(); len];
    receive_block(usrp, sfreq, srate, &stream_args, &mut narrow)?;
    let rms8 = component_rms(&narrow);

    // UHD's documented relation, used only if the measurement cannot be:
    // no signal at all in the 8-bit snapshot.
    let expected_scale = peak * 256.0;
    let scale = if rms8 > 0.0 && rms16 > 0.0 {
        rms16 / rms8
    } else {
        warn!("8-bit level at {address}: no signal to measure, assuming UHD's scaling");
        expected_scale
    };
    info!(
        "8-bit level at {address}: {rms16:.0} counts RMS in 16-bit, peak {peak:.4}, \
         {rms8:.1} counts RMS in 8-bit; scale {scale:.2} (UHD's {expected_scale:.2})"
    );
    Ok(EightBitLevel {
        stream_args,
        full_scale: 32768.0 / scale,
    })
}

/// Turns a block of raw samples into a power spectrum of `avg_pts` channels,
/// RFI-clipped if asked. Pure CPU work, run on the processing thread. Keeps the
/// FFT plan and work buffers between blocks rather than rebuilding them for
/// every one.
struct SpectrumProcessor {
    fft: Arc<dyn rustfft::Fft<f64>>,
    fft_pts: usize,
    avg_pts: usize,
    rfi_filter: bool,
    fft_buffer: Vec<Complex<f64>>,
    fft_abs: Vec<f64>,
}

impl SpectrumProcessor {
    fn new(fft_pts: usize, avg_pts: usize, rfi_filter: bool) -> Self {
        Self {
            fft: FftPlanner::new().plan_fft_forward(fft_pts),
            fft_pts,
            avg_pts,
            rfi_filter,
            fft_buffer: vec![Complex::default(); fft_pts],
            fft_abs: vec![0.0; fft_pts],
        }
    }

    /// `full_scale` is the count value corresponding to ADC full scale:
    /// 32768 for 16-bit samples, and the measured equivalent for 8-bit ones
    /// (see [`auto_level`]).
    fn spectrum<T: RawSample>(&mut self, samples: &[Complex<T>], full_scale: f64) -> Vec<f64> {
        let fft_pts = self.fft_pts;
        let nstack: usize = samples.len() / fft_pts;
        let navg: usize = fft_pts / self.avg_pts;

        // Accumulate power spectrum (|FFT|^2) across stacked blocks
        self.fft_abs.fill(0.0);
        // Loop through the samples, taking fft_pts each time. We rescale to
        // ±1.0 (fraction of ADC full scale) before the FFT — otherwise |FFT|²
        // lands in the 10⁹+ range purely from the digitiser's count scale,
        // which is not a physical unit.
        for block in samples.chunks_exact(fft_pts).take(nstack) {
            for (out, x) in zip(self.fft_buffer.iter_mut(), block) {
                let (re, im): (f64, f64) = (x.re.into(), x.im.into());
                *out = Complex::<f64>::new(re / full_scale, im / full_scale);
            }
            // Do the FFT
            self.fft.process(&mut self.fft_buffer);
            // Accumulate squared magnitudes (Welch's method).
            // Seems the pos/neg halves of spectrum are flipped, so reflip them
            // we want lowest frequency in element 0 and then increasing
            for i in 0..fft_pts / 2 {
                self.fft_abs[i + fft_pts / 2] += self.fft_buffer[i].norm_sqr();
                self.fft_abs[i] += self.fft_buffer[i + fft_pts / 2].norm_sqr();
            }
        }
        // Average over the stacked blocks
        for val in self.fft_abs.iter_mut() {
            *val /= nstack as f64;
        }

        // Average spectrum to save data
        let mut fft_avg: Vec<f64> = (0..self.avg_pts)
            .map(|i| self.fft_abs.iter().skip(navg * i).take(navg).sum::<f64>() / navg as f64)
            .collect();

        if self.rfi_filter {
            clip_rfi_spikes(&mut fft_avg);
        }
        fft_avg
    }
}

/// The processing half of [`measure`]: turns each received cycle into a
/// spectrum and folds it into the running average, until the receiving side
/// hangs up and the queue is drained. Hands sample buffers back through
/// `spare` so the receiver can reuse them instead of allocating ~100 MB a
/// second at 25 MHz.
///
/// Also counts I and Q values at the format's limits, which in 8-bit means the
/// level was set too high for this part of the run: a reference half much
/// louder than the signal half, or strong RFI turning up partway.
#[allow(clippy::too_many_arguments)]
fn process_cycles<T: RawSample>(
    cycles: std::sync::mpsc::Receiver<CycleSamples<T>>,
    spare: std::sync::mpsc::Sender<Vec<Complex<T>>>,
    measurements: Arc<Mutex<Vec<Measurement>>>,
    mut processor: SpectrumProcessor,
    full_scale: f64,
    tsys: f64,
    tint: f64,
    address: &str,
) {
    let mut n = 0.0;
    let mut at_limit: usize = 0;
    let mut values: usize = 0;
    let mut warned = false;
    for cycle in cycles {
        let cycle_values = 2 * (cycle.sig.len() + cycle.reference.as_ref().map_or(0, Vec::len));
        let cycle_at_limit =
            count_at_limit(&cycle.sig) + cycle.reference.as_deref().map_or(0, count_at_limit);
        at_limit += cycle_at_limit;
        values += cycle_values;
        if !warned && cycle_at_limit as f64 > CLIPPING_WARN_FRACTION * cycle_values as f64 {
            warn!(
                "Receiver at {address} is clipping: {cycle_at_limit} of {cycle_values} {}-bit \
                 values in one cycle at the limit",
                8 * size_of::<T>()
            );
            warned = true;
        }

        let sig = processor.spectrum(&cycle.sig, full_scale);
        // The receiver may already have gone; then the buffer is just freed.
        let _ = spare.send(cycle.sig);
        let spec: Vec<f64> = match cycle.reference {
            Some(reference) => {
                let reference_spec = processor.spectrum(&reference, full_scale);
                let _ = spare.send(reference);
                // Form sig-ref difference and scale with Tsys
                zip(&sig, &reference_spec)
                    .map(|(s, r)| tsys * (s - r) / r)
                    .collect()
            }
            None => sig,
        };
        n += 1.0;

        let mut measurements = measurements.blocking_lock();
        let Some(measurement) = measurements.last_mut() else {
            break;
        };
        for (amp, spec_val) in zip(measurement.amps.iter_mut(), spec.iter()) {
            *amp = (*amp * (n - 1.0) + spec_val) / n;
        }
        // Sample time integrated, not wall clock: `n` completed cycles of
        // `tint` each. The wall-clock span also covers the retune and stream
        // setup between blocks, which contribute no samples and so would
        // overstate the integration.
        measurement.duration = Duration::from_secs_f64(n * tint);
    }
    if warned {
        warn!(
            "Receiver at {address} clipped {at_limit} of {values} values over the run ({:.3} %)",
            100.0 * at_limit as f64 / values.max(1) as f64
        );
    }
}

/// Fraction of values at the format's limit in one cycle above which the run
/// is reported as clipping. Noise at the 8-bit target level reaches the limit
/// about once in 10⁹ values, so this only trips on a real problem.
const CLIPPING_WARN_FRACTION: f64 = 1e-4;

/// Robust narrow-band RFI clipper for an averaged power spectrum.
///
/// The previous chunk-aligned, percentage-of-median clip had three failure
/// modes: a 32-channel chunk centred on a spike could shift its own median;
/// the 10 % threshold scaled with the bandpass envelope rather than with
/// noise level; and clipping only the central bin left adjacent leakage
/// bins unfiltered, producing a residual lump after averaging.
///
/// This version, in two passes:
///   1. Compute a wide running-median baseline. The window is set well
///      beyond any expected real emission (HI is at most ~20 channels
///      wide on the default 512-channel / 2.5 MHz output) so the median
///      is dominated by continuum even inside an emission feature.
///   2. For each channel, compute the residual against the baseline,
///      then estimate the local noise from an annular neighbourhood
///      (channels at distance `STAT_INNER..=STAT_OUTER`). Excluding the
///      candidate's immediate neighbours keeps FFT-leakage bins out of
///      its own noise estimate.
///   3. Flag channels with residual > K_SIGMA * MAD-σ. Replace the
///      channel and ±REPLACE_PAD neighbours (leakage) with the baseline
///      value at that bin.
///
/// Two passes catch small spikes whose statistics were masked by larger
/// ones cleaned in pass one.
fn clip_rfi_spikes(spec: &mut [f64]) {
    const BASELINE_HW: usize = 64;
    const STAT_INNER: usize = 4;
    const STAT_OUTER: usize = 24;
    const K_SIGMA: f64 = 4.5;
    const REPLACE_PAD: usize = 1;
    const PASSES: usize = 2;

    let n = spec.len();
    if n == 0 {
        return;
    }
    for _ in 0..PASSES {
        let baseline: Vec<f64> = (0..n)
            .map(|i| {
                let lo = i.saturating_sub(BASELINE_HW);
                let hi = (i + BASELINE_HW + 1).min(n);
                median(spec[lo..hi].to_vec())
            })
            .collect();
        let residual: Vec<f64> = (0..n).map(|i| spec[i] - baseline[i]).collect();
        let mut flagged = vec![false; n];
        for i in 0..n {
            let lo = i.saturating_sub(STAT_OUTER);
            let hi = (i + STAT_OUTER + 1).min(n);
            let ann: Vec<f64> = (lo..hi)
                .filter(|&j| {
                    let d = j.abs_diff(i);
                    d > STAT_INNER && d <= STAT_OUTER
                })
                .map(|j| residual[j])
                .collect();
            if ann.is_empty() {
                continue;
            }
            let m = median(ann.clone());
            let mad = median(ann.iter().map(|x| (x - m).abs()).collect()) * 1.4826;
            if mad == 0.0 {
                continue;
            }
            if residual[i] - m > K_SIGMA * mad {
                let plo = i.saturating_sub(REPLACE_PAD);
                let phi = (i + REPLACE_PAD + 1).min(n);
                for slot in &mut flagged[plo..phi] {
                    *slot = true;
                }
            }
        }
        for i in 0..n {
            if flagged[i] {
                spec[i] = baseline[i];
            }
        }
    }
}

fn median(mut xs: Vec<f64>) -> f64 {
    if xs.is_empty() {
        return 0.0;
    }
    xs.sort_by(|x, y| x.partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal));
    let n = xs.len();
    if n.is_multiple_of(2) {
        (xs[n / 2] + xs[n / 2 - 1]) / 2.0
    } else {
        xs[n / 2]
    }
}

// Sync function intended to run on tokio's blocking thread pool via
// `spawn_blocking`. The hot loop is pure FFI (`receive_simple`) and CPU work
// (FFT, RFI filter); running it on an async worker would block one of the
// shared runtime threads for ~1 s per iteration and starve unrelated requests.
fn measure(
    address: String,
    measurements: Arc<Mutex<Vec<Measurement>>>,
    cancellation_token: CancellationToken,
    config: ReceiverConfiguration,
    tsys_k: f64,
    device_lock: DeviceLock,
) -> Result<(), TelescopeError> {
    let tint: f64 = 1.0; // integration time per cycle, seconds
    let srate: f64 = config.bandwidth_hz;
    let sfreq: f64 = config.center_freq_hz;
    let rfreq: f64 = config.ref_freq_hz;
    let avg_pts: usize = config.spectral_channels.max(1);
    let fft_pts: usize = 8192; // ^2 Number of points in FFT, setting spectral resolution
    let gain: f64 = config.gain_db;
    let mode = config.mode;

    // Setup usrp for taking data. Opening and configuring the device takes
    // about a second on an N210 and none of it is cancellation-aware, so a
    // stop arriving during startup has to wait it out. Check on the way in
    // and again on the way out, so at least a stop that lands before the
    // open returns immediately rather than paying for a device we are about
    // to abandon.
    if cancellation_token.is_cancelled() {
        return Ok(());
    }
    // Declared before `usrp` so it is released after it: locals drop in reverse
    // declaration order, and the guard's whole job is to outlive the device
    // handle it protects. Waiting here can consume most of a stop, so re-check
    // the token once the receiver is ours.
    let _device = acquire_device(&device_lock, &address)?;
    if cancellation_token.is_cancelled() {
        return Ok(());
    }
    let mut usrp = UhdDevice::open(&address)
        .map_err(|e| TelescopeError::ReceiverFailed(format!("open USRP at {address}: {e}")))?;

    // The N210 only has one input channel 0. Empty string sets all gains.
    usrp.with(|u| u.set_rx_gain(gain, 0, ""))
        .map_err(|e| TelescopeError::ReceiverFailed(format!("set_rx_gain: {e}")))?;
    usrp.with(|u| u.set_rx_antenna("TX/RX", 0))
        .map_err(|e| TelescopeError::ReceiverFailed(format!("set_rx_antenna: {e}")))?;
    usrp.with(|u| u.set_rx_dc_offset_enabled(true, 0))
        .map_err(|e| TelescopeError::ReceiverFailed(format!("set_rx_dc_offset_enabled: {e}")))?;
    usrp.with(|u| u.set_rx_sample_rate(srate, 0))
        .map_err(|e| TelescopeError::ReceiverFailed(format!("set_rx_sample_rate: {e}")))?;
    // LO-offset tuning (see receive_block) moves the wanted band away
    // from the analog baseband centre; open the DBSRX2 filter fully so
    // the band sits on the flat part of its response.
    usrp.with(|u| u.set_rx_bandwidth(DBSRX2_BANDWIDTH_HZ, 0))
        .map_err(|e| TelescopeError::ReceiverFailed(format!("set_rx_bandwidth: {e}")))?;

    let tsys = tsys_k;
    info!("Tsys = {:.1} K (from config)", tsys);

    // Nothing has been integrated yet, so leaving now costs no data. Bail
    // before pushing the measurement rather than after: a pushed but empty
    // measurement is what `stop_integration` hands back, and a zero-cycle
    // spectrum used to reach the archive as a 0 s observation.
    if cancellation_token.is_cancelled() {
        return Ok(());
    }

    {
        let mut measurements = measurements.blocking_lock();
        let mut measurement = Measurement {
            amps: vec![0.0; avg_pts],
            freqs: vec![0.0; avg_pts],
            start: Utc::now(),
            duration: Duration::from_secs(0),
        };
        for i in 0..avg_pts {
            measurement.freqs[i] = sfreq - 0.5 * srate + srate * (i as f64 / avg_pts as f64);
        }
        measurements.push(measurement);
    }

    let switched = match mode {
        ObservationMode::FreqSwitched => true,
        ObservationMode::Raw => false,
        ObservationMode::Interferometry => return Ok(()),
    };

    // Fixed-duration mode: run a whole number of cycles covering the request,
    // rounding up so the observer never gets less time than they asked for.
    // Enforced here rather than by cancelling from outside, because a cancel
    // landing mid-cycle cannot shorten the block: Raw mode has no abort point
    // inside a block, so the block completes and is averaged in anyway, one
    // full cycle past the target.
    let max_cycles = config
        .max_duration
        .map(|target| ((target.as_secs_f64() / tint).ceil() as u64).max(1));

    let settings = PipelineSettings {
        sfreq,
        rfreq,
        srate,
        tint,
        tsys,
        fft_pts,
        avg_pts,
        rfi_filter: config.rfi_filter,
        switched,
        max_cycles,
    };
    if sample_bits(srate) == 8 {
        let level = auto_level(&mut usrp, &address, sfreq, srate)?;
        run_pipeline::<i8>(
            &mut usrp,
            &address,
            measurements,
            &cancellation_token,
            settings,
            &level.stream_args,
            level.full_scale,
        )
    } else {
        run_pipeline::<i16>(
            &mut usrp,
            &address,
            measurements,
            &cancellation_token,
            settings,
            "",
            32768.0,
        )
    }
}

/// What [`run_pipeline`] needs to know about the run.
#[derive(Clone, Copy)]
struct PipelineSettings {
    sfreq: f64,
    rfreq: f64,
    srate: f64,
    tint: f64,
    tsys: f64,
    fft_pts: usize,
    avg_pts: usize,
    rfi_filter: bool,
    switched: bool,
    max_cycles: Option<u64>,
}

/// Receive cycles of `T` samples until the run is done or stopped, while a
/// second thread turns them into the averaged spectrum. `stream_args` and
/// `full_scale` set and undo the 8-bit scaling; `""` and 32768 for 16-bit.
fn run_pipeline<T: RawSample>(
    usrp: &mut UhdDevice,
    address: &str,
    measurements: Arc<Mutex<Vec<Measurement>>>,
    cancellation_token: &CancellationToken,
    settings: PipelineSettings,
    stream_args: &str,
    full_scale: f64,
) -> Result<(), TelescopeError>
where
    Complex<T>: uhd::Item,
{
    let PipelineSettings {
        sfreq,
        rfreq,
        srate,
        tint,
        tsys,
        fft_pts,
        avg_pts,
        rfi_filter,
        switched,
        max_cycles,
    } = settings;

    // Receiving and processing run side by side: this thread only tunes and
    // records, and a second thread does the FFT, RFI filter and averaging.
    // Done in sequence, the receiver sat idle during processing, which grows
    // with the channel count — at 8192 channels with the RFI filter only 86 %
    // of wall-clock time became data (torre and vale, 2026-10-30).
    let (cycle_tx, cycle_rx) = std::sync::mpsc::sync_channel::<CycleSamples<T>>(PIPELINE_DEPTH);
    let (spare_tx, spare_rx) = std::sync::mpsc::channel::<Vec<Complex<T>>>();
    let processor_thread = {
        let processor = SpectrumProcessor::new(fft_pts, avg_pts, rfi_filter);
        let address = address.to_string();
        std::thread::Builder::new()
            .name("salsa-spectrum".to_string())
            .spawn(move || {
                process_cycles(
                    cycle_rx,
                    spare_tx,
                    measurements,
                    processor,
                    full_scale,
                    tsys,
                    tint,
                    &address,
                )
            })
            .map_err(|e| {
                TelescopeError::ReceiverFailed(format!("start spectrum processing thread: {e}"))
            })?
    };

    let block_secs = if switched { 0.5 * tint } else { tint };
    let block_len = (block_secs * srate) as usize;
    let next_buffer = || {
        let mut buffer = spare_rx.try_recv().unwrap_or_default();
        buffer.resize(block_len, Complex::default());
        buffer
    };

    // Counts only cycles handed to the processor, so a skipped one never
    // counts towards the requested duration.
    let mut queued: u64 = 0;
    let mut skipped: u64 = 0;
    let received: Result<(), TelescopeError> = (|| {
        while !cancellation_token.is_cancelled() && !max_cycles.is_some_and(|max| queued >= max) {
            let mut sig = next_buffer();
            receive_block(usrp, sfreq, srate, stream_args, &mut sig)?;
            let reference = if switched {
                // Bail between sig and ref if a stop has been requested, so the
                // user does not wait out the rest of the cycle. The half cycle
                // is dropped. (Raw mode has no such halfway point.)
                if cancellation_token.is_cancelled() {
                    break;
                }
                let mut reference = next_buffer();
                receive_block(usrp, rfreq, srate, stream_args, &mut reference)?;
                Some(reference)
            } else {
                None
            };
            match cycle_tx.try_send(CycleSamples { sig, reference }) {
                Ok(()) => queued += 1,
                // Processing has fallen behind. Dropping this cycle keeps the
                // receiver going; the run just takes a little longer.
                Err(std::sync::mpsc::TrySendError::Full(_)) => skipped += 1,
                // The processing thread has died; the join below reports it.
                Err(std::sync::mpsc::TrySendError::Disconnected(_)) => break,
            }
        }
        Ok(())
    })();

    // Hanging up lets the processor drain what is queued and finish.
    drop(cycle_tx);
    let processed = processor_thread.join();
    if skipped > 0 {
        warn!(
            "Skipped {skipped} of {} received cycles because spectrum processing fell behind",
            queued + skipped
        );
    }
    received?;
    processed.map_err(|_| {
        TelescopeError::ReceiverFailed("spectrum processing thread panicked".to_string())
    })
}

/// Stream raw IQ blocks for interferometry cross-correlation.
/// Sends blocks of IQ_BLOCK_SIZE Complex<f32> samples over `tx`.
/// Exits when `cancellation_token` fires or the receiver drops the channel.
///
/// Sync function intended to run via `spawn_blocking` — see `measure()`.
fn measure_iq(
    address: String,
    gpsdo_enabled: bool,
    cancellation_token: CancellationToken,
    config: ReceiverConfiguration,
    tx: tokio::sync::mpsc::Sender<IqBlock>,
    device_lock: DeviceLock,
) -> Result<(), TelescopeError> {
    // Before `usrp`, so it is released after the device — see `measure()`.
    let _device = acquire_device(&device_lock, &address)?;
    if cancellation_token.is_cancelled() {
        return Ok(());
    }
    let mut usrp = UhdDevice::open(&address).map_err(|e| {
        TelescopeError::ReceiverFailed(format!("IQ stream: open USRP at {address}: {e}"))
    })?;

    if gpsdo_enabled {
        info!("Configuring external clock/PPS sync for {}", address);
        usrp.with(|u| u.set_clock_source("external", 0))
            .map_err(|e| TelescopeError::ReceiverFailed(format!("set_clock_source: {e}")))?;
        usrp.with(|u| u.set_time_source("external", 0))
            .map_err(|e| TelescopeError::ReceiverFailed(format!("set_time_source: {e}")))?;
        // Latch t=0 on the next PPS edge, then wait for it to settle.
        usrp.with(|u| u.set_time_next_pps(0, 0.0, 0))
            .map_err(|e| TelescopeError::ReceiverFailed(format!("set_time_next_pps: {e}")))?;
        std::thread::sleep(Duration::from_secs(2));
        info!("External time sync complete for {}", address);
    }

    usrp.with(|u| u.set_rx_gain(config.gain_db, 0, ""))
        .map_err(|e| TelescopeError::ReceiverFailed(format!("set_rx_gain: {e}")))?;
    usrp.with(|u| u.set_rx_antenna("TX/RX", 0))
        .map_err(|e| TelescopeError::ReceiverFailed(format!("set_rx_antenna: {e}")))?;
    usrp.with(|u| u.set_rx_dc_offset_enabled(true, 0))
        .map_err(|e| TelescopeError::ReceiverFailed(format!("set_rx_dc_offset_enabled: {e}")))?;
    usrp.with(|u| u.set_rx_sample_rate(config.bandwidth_hz, 0))
        .map_err(|e| TelescopeError::ReceiverFailed(format!("set_rx_sample_rate: {e}")))?;
    // Same LO-offset tuning and wide-open analog filter as the
    // single-dish path (see receive_block): keeps the direct-conversion
    // DC artifact out of the correlated band. Both telescopes in a session
    // get the identical offset, so the inter-device phase relation is
    // unchanged.
    usrp.with(|u| u.set_rx_bandwidth(DBSRX2_BANDWIDTH_HZ, 0))
        .map_err(|e| TelescopeError::ReceiverFailed(format!("set_rx_bandwidth: {e}")))?;
    usrp.with(|u| {
        u.set_rx_frequency(
            &TuneRequest::with_frequency_lo(
                config.center_freq_hz,
                lo_offset_hz(config.bandwidth_hz),
            ),
            0,
        )
    })
    .map_err(|e| TelescopeError::ReceiverFailed(format!("set_rx_frequency: {e}")))?;

    // Open a single continuous stream once and pull IQ_BLOCK_SIZE samples at a time.
    // Re-tuning / reopening the stream per block would invalidate cross-correlation
    // because each setup introduces a fresh, uncorrelated time gap between telescopes.
    let mut receiver = usrp
        .with(|u| u.get_rx_stream(&uhd::StreamArgs::<Complex<i16>>::new("sc16")))
        .map_err(|e| TelescopeError::ReceiverFailed(format!("get_rx_stream: {e}")))?;
    receiver
        .send_command(&StreamCommand {
            command_type: StreamCommandType::StartContinuous,
            time: StreamTime::Now,
        })
        .map_err(|e| TelescopeError::ReceiverFailed(format!("StartContinuous: {e}")))?;

    let mut buffer = vec![Complex::<i16>::default(); IQ_BLOCK_SIZE];
    // Fallback sample counter, in case the USRP metadata omits a time_spec on
    // some packets. Both telescopes increment at the same rate, so if they both
    // lose metadata, sample-count alignment still works as long as the initial
    // offsets match.
    let mut samples_received: u64 = 0;
    while !cancellation_token.is_cancelled() {
        let metadata = match receiver.receive_simple(buffer.as_mut()) {
            Ok(m) => m,
            Err(_) => break,
        };

        // Surface UHD receive errors (overflow, out-of-sequence, ...) — these
        // indicate the pipeline is keeping up poorly and cross-correlation
        // alignment is likely compromised. Keep streaming; the correlator's
        // timestamp-alignment check will reset the integration on next drift.
        if let Some(err) = metadata.last_error() {
            warn!("IQ stream {address}: receive error {err:?}");
        }

        let timestamp_secs = match metadata.time_spec() {
            Some(ts) => ts.seconds as f64 + ts.fraction,
            None => {
                warn!(
                    "IQ stream from {}: receive packet had no time_spec; using sample counter",
                    address
                );
                samples_received as f64 / config.bandwidth_hz
            }
        };
        samples_received += IQ_BLOCK_SIZE as u64;

        let samples: Vec<Complex<f32>> = buffer
            .iter()
            .map(|x| Complex::new(x.re as f32, x.im as f32))
            .collect();

        if tx
            .blocking_send(IqBlock {
                timestamp_secs,
                samples,
            })
            .is_err()
        {
            break;
        }
    }

    if let Err(e) = receiver.send_command(&StreamCommand {
        command_type: StreamCommandType::StopContinuous,
        time: StreamTime::Now,
    }) {
        // Don't fail the whole stream just because the stop command didn't take.
        warn!("IQ stream: StopContinuous failed: {}", e);
    }
    Ok(())
}

#[cfg(test)]
mod test {
    use super::*;

    /// Deterministic pseudo-noise samples, so every spectral channel is nonzero.
    fn noise_samples(len: usize, seed: u64) -> Vec<Complex<i16>> {
        let mut state = seed;
        let mut next = move || {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((state >> 48) as i16) / 8
        };
        (0..len).map(|_| Complex::new(next(), next())).collect()
    }

    fn empty_measurement(avg_pts: usize) -> Arc<Mutex<Vec<Measurement>>> {
        Arc::new(Mutex::new(vec![Measurement {
            amps: vec![0.0; avg_pts],
            freqs: vec![0.0; avg_pts],
            start: Utc::now(),
            duration: Duration::ZERO,
        }]))
    }

    #[test]
    fn a_tone_above_centre_lands_in_the_upper_half_of_the_spectrum() {
        // Positive baseband frequency is above the tuned centre, so it must
        // land above the middle channel: element 0 is the lowest frequency.
        let (fft_pts, avg_pts, bin) = (8192, 512, 1000);
        let samples: Vec<Complex<i16>> = (0..4 * fft_pts)
            .map(|t| {
                let phase = 2.0 * std::f64::consts::PI * (bin * t) as f64 / fft_pts as f64;
                Complex::new((8000.0 * phase.cos()) as i16, (8000.0 * phase.sin()) as i16)
            })
            .collect();
        let spectrum = SpectrumProcessor::new(fft_pts, avg_pts, false).spectrum(&samples, 32768.0);
        let peak = spectrum
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .map(|(i, _)| i)
            .unwrap();
        assert_eq!(peak, (fft_pts / 2 + bin) / (fft_pts / avg_pts));
    }

    #[test]
    fn processed_cycles_average_into_the_measurement() {
        let (fft_pts, avg_pts, tint) = (64, 8, 1.0);
        let blocks = [noise_samples(256, 1), noise_samples(256, 2)];
        let mut reference = SpectrumProcessor::new(fft_pts, avg_pts, false);
        let expected: Vec<f64> = zip(
            reference.spectrum(&blocks[0], 32768.0),
            reference.spectrum(&blocks[1], 32768.0),
        )
        .map(|(a, b)| (a + b) / 2.0)
        .collect();

        let measurements = empty_measurement(avg_pts);
        let (cycle_tx, cycle_rx) = std::sync::mpsc::sync_channel(PIPELINE_DEPTH);
        let (spare_tx, spare_rx) = std::sync::mpsc::channel();
        for sig in blocks.clone() {
            cycle_tx
                .send(CycleSamples {
                    sig,
                    reference: None,
                })
                .unwrap();
        }
        drop(cycle_tx);
        let processor = SpectrumProcessor::new(fft_pts, avg_pts, false);
        process_cycles(
            cycle_rx,
            spare_tx,
            measurements.clone(),
            processor,
            32768.0,
            300.0,
            tint,
            "test",
        );

        let measurements = measurements.blocking_lock();
        let measurement = &measurements[0];
        assert_eq!(measurement.duration, Duration::from_secs(2));
        for (got, want) in zip(&measurement.amps, &expected) {
            assert!((got - want).abs() <= 1e-12 * want.abs(), "{got} != {want}");
        }
        assert_eq!(
            spare_rx.try_iter().count(),
            2,
            "both sample buffers should be handed back for reuse"
        );
    }

    #[test]
    fn a_switched_cycle_is_scaled_against_its_reference() {
        let (fft_pts, avg_pts, tsys) = (64, 8, 300.0);
        let (sig, reference) = (noise_samples(256, 3), noise_samples(256, 4));
        let mut direct = SpectrumProcessor::new(fft_pts, avg_pts, false);
        let (s, r) = (
            direct.spectrum(&sig, 32768.0),
            direct.spectrum(&reference, 32768.0),
        );
        let expected: Vec<f64> = zip(&s, &r).map(|(s, r)| tsys * (s - r) / r).collect();

        let measurements = empty_measurement(avg_pts);
        let (cycle_tx, cycle_rx) = std::sync::mpsc::sync_channel(PIPELINE_DEPTH);
        let (spare_tx, spare_rx) = std::sync::mpsc::channel();
        cycle_tx
            .send(CycleSamples {
                sig,
                reference: Some(reference),
            })
            .unwrap();
        drop(cycle_tx);
        let processor = SpectrumProcessor::new(fft_pts, avg_pts, false);
        process_cycles(
            cycle_rx,
            spare_tx,
            measurements.clone(),
            processor,
            32768.0,
            tsys,
            1.0,
            "test",
        );

        let measurements = measurements.blocking_lock();
        assert_eq!(measurements[0].amps, expected);
        assert_eq!(measurements[0].duration, Duration::from_secs(1));
        assert_eq!(spare_rx.try_iter().count(), 2);
    }

    // An 8-bit block scaled by k must give the same spectrum as the 16-bit
    // block it stands for, so raw-mode power stays on one absolute scale
    // whichever bit depth the bandwidth uses.
    #[test]
    fn an_eight_bit_block_gives_the_same_spectrum_as_sixteen_bits() {
        let (fft_pts, avg_pts, scale) = (64, 8, 16.0);
        let narrow: Vec<Complex<i8>> = noise_samples(256, 5)
            .iter()
            .map(|x| Complex::new((x.re / 64) as i8, (x.im / 64) as i8))
            .collect();
        let wide: Vec<Complex<i16>> = narrow
            .iter()
            .map(|x| Complex::new(x.re as i16 * scale as i16, x.im as i16 * scale as i16))
            .collect();
        let mut processor = SpectrumProcessor::new(fft_pts, avg_pts, false);
        let from_wide = processor.spectrum(&wide, 32768.0);
        let from_narrow = processor.spectrum(&narrow, 32768.0 / scale);
        for (a, b) in zip(&from_wide, &from_narrow) {
            assert!((a - b).abs() <= 1e-12 * a.abs(), "{a} != {b}");
        }
    }

    // The 8-bit scaling puts the signal at the target level. Torre's GNSS
    // reference spectra of 2026-11-01 sit at ~1060 counts RMS in 16-bit.
    #[test]
    fn the_eight_bit_scaling_puts_the_signal_at_the_target_level() {
        let peak = eight_bit_peak(1060.0);
        assert!((peak - 0.207).abs() < 0.001, "peak {peak}");
        assert!((1060.0 / (peak * 256.0) - EIGHT_BIT_TARGET_RMS).abs() < 1e-9);
        // UHD's limits: no finer than 1/256, no coarser than full scale.
        assert_eq!(eight_bit_peak(0.0), 1.0 / 256.0);
        assert_eq!(eight_bit_peak(32768.0), 1.0);
    }

    #[test]
    fn values_at_the_format_limit_are_counted_as_clipped() {
        let samples = [
            Complex::new(127i8, 0),
            Complex::new(-128, 3),
            Complex::new(126, -127),
            Complex::new(-128, 127),
        ];
        assert_eq!(count_at_limit(&samples), 4);
        assert_eq!(count_at_limit(&[Complex::new(i16::MAX, 5)]), 1);
    }

    #[test]
    fn component_rms_is_per_i_or_q() {
        let samples = [Complex::new(3i16, 4), Complex::new(-3, -4)];
        assert!((component_rms(&samples) - (12.5f64).sqrt()).abs() < 1e-12);
    }

    #[test]
    fn clip_rfi_keeps_broad_signal() {
        // Synthetic 512-channel power spectrum modelled on vale's sig.csv
        // (May 2026 RFI test data): bandpass envelope, deterministic
        // pseudo-noise, a 21-channel HI emission feature, and a giant
        // narrow RFI spike.
        let n = 512;
        let noise = |i: usize| -> f64 {
            let f = i as f64;
            (f * 0.713).sin() * 8.0 + (f * 1.317).cos() * 7.0 + (f * 0.209).sin() * 6.0
        };
        let mut spec: Vec<f64> = (0..n)
            .map(|i| {
                let x = (i as f64 - n as f64 / 2.0) / (n as f64 / 2.0);
                325.0 + 50.0 * (1.0 - x * x).max(0.0).sqrt() + noise(i)
            })
            .collect();
        for v in &mut spec[235..=255] {
            *v += 40.0;
        }
        spec[173] = 2473.0;

        let original = spec.clone();
        clip_rfi_spikes(&mut spec);

        // The giant narrow spike must be clipped down close to baseline.
        assert!(
            spec[173] < 500.0,
            "giant spike at 173 not clipped (was {}, now {})",
            original[173],
            spec[173],
        );
        // The broad HI feature must survive — peak preserved within a few percent.
        let hi_peak_before = original[235..=255].iter().cloned().fold(f64::MIN, f64::max);
        let hi_peak_after = spec[235..=255].iter().cloned().fold(f64::MIN, f64::max);
        assert!(
            hi_peak_after > hi_peak_before * 0.95,
            "HI feature degraded (peak was {hi_peak_before}, now {hi_peak_after})",
        );
    }

    #[test]
    fn second_integration_waits_for_the_receiver_to_be_released() {
        // The vale crash of 2026-08-28: a start landing while the previous
        // measurement task still holds the device must not open a second UHD
        // session on it. See `DeviceLock`.
        let lock: DeviceLock = Arc::new(std::sync::Mutex::new(()));

        let held = lock.lock().unwrap();
        let contender = lock.clone();
        let waiter = std::thread::spawn(move || {
            acquire_device_within(&contender, "192.168.1.1", Duration::from_secs(10)).is_ok()
        });

        // Long enough for the waiter to have gone round its poll loop several
        // times while the device is still taken.
        std::thread::sleep(Duration::from_millis(200));
        assert!(!waiter.is_finished(), "took the receiver while it was held");

        drop(held);
        assert!(
            waiter.join().expect("waiter panicked"),
            "did not take the receiver once it was released"
        );
    }

    #[test]
    fn uhd_registry_is_held_exclusively_to_open_and_free_and_shared_for_calls() {
        // The crashes of 2026-10-02 and 2026-10-09: one receiver opening while
        // another was freed raced on UHD's global device map. See
        // `UHD_REGISTRY`. One test, not three, because the lock is a process
        // global and parallel tests holding it would make the probes flaky.
        let registry_is_exclusive = || UHD_REGISTRY.try_read().is_err();
        let registry_is_shared =
            || UHD_REGISTRY.try_read().is_ok() && UHD_REGISTRY.try_write().is_err();

        struct FakeDevice(Arc<std::sync::atomic::AtomicBool>);
        impl Drop for FakeDevice {
            fn drop(&mut self) {
                self.0.store(
                    UHD_REGISTRY.try_read().is_err(),
                    std::sync::atomic::Ordering::SeqCst,
                );
            }
        }

        let freed_exclusively = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let mut opened_exclusively = false;
        let mut device = UhdDevice::open_with("192.168.1.1", || {
            opened_exclusively = registry_is_exclusive();
            Ok::<_, ()>(FakeDevice(freed_exclusively.clone()))
        })
        .unwrap();
        assert!(
            opened_exclusively,
            "opened a device without the exclusive lock"
        );
        assert!(UHD_REGISTRY.try_write().is_ok(), "open kept the lock");

        assert!(
            device.with(|_| registry_is_shared()),
            "made a device call without the shared lock"
        );
        assert!(
            UHD_REGISTRY.try_write().is_ok(),
            "a device call kept the lock"
        );

        drop(device);
        assert!(
            freed_exclusively.load(std::sync::atomic::Ordering::SeqCst),
            "freed a device without the exclusive lock"
        );
    }

    #[test]
    fn waiting_for_the_receiver_gives_up_rather_than_hanging() {
        let lock: DeviceLock = Arc::new(std::sync::Mutex::new(()));
        let _held = lock.lock().unwrap();
        let result = acquire_device_within(&lock, "192.168.1.1", Duration::from_millis(100));
        assert!(result.is_err(), "waited out a wedged session forever");
    }

    #[test]
    fn a_panicked_measurement_does_not_lock_the_receiver_out() {
        // A task that panics with the device open poisons the mutex, but the
        // `Usrp` was dropped as the panic unwound, so the receiver is free.
        let lock: DeviceLock = Arc::new(std::sync::Mutex::new(()));
        let poisoner = lock.clone();
        let _ = std::thread::spawn(move || {
            let _guard = poisoner.lock().unwrap();
            panic!("measurement task died holding the device");
        })
        .join();
        assert!(lock.is_poisoned());

        assert!(
            acquire_device_within(&lock, "192.168.1.1", Duration::from_millis(100)).is_ok(),
            "a poisoned lock left the receiver permanently unusable"
        );
    }

    #[test]
    fn test_rot2prog_bytes_to_angle_documented() {
        // This behavior is what I expect reading the documentation, but the telescope seems to work with returned bytes
        // directly instead of ascii encoded numbers. E.g. 0x03 instead of 0x33 which is '3' in ascii.
        assert!(
            (rot2prog_bytes_to_angle_documented(&[0x33, 0x36, 0x30, 0x30, 0x30]) - 0.0).abs()
                < 0.01,
        );
        // Example from documentation
        assert!(
            (rot2prog_bytes_to_angle_documented(&[0x33, 0x38, 0x32, 0x33, 0x33,])
                - 22.33_f64.to_radians())
            .abs()
                < 0.01,
        );
    }
}
