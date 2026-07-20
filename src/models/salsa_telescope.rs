use crate::coords::{Direction, Location};
use crate::models::telescope::Telescope;
use crate::models::telescope_types::{
    CalibrationResult, IQ_BLOCK_SIZE, IqBlock, Measurement, ObservationMode, ObservedSpectra,
    ReceiverConfiguration, ReceiverError, TelescopeError, TelescopeInfo, TelescopeTarget,
};
use crate::telescope_tracker::TelescopeTracker;
use crate::tle_cache::TleCacheHandle;
use async_trait::async_trait;
use chrono::Utc;
use std::iter::zip;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

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
    measurement_task: tokio::task::JoinHandle<Result<(), TelescopeError>>,
    kind: IntegrationKind,
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
        inner
            .controller
            .set_target(target, az_offset_rad, el_offset_rad)
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
            inner.receiver_configuration.integrate = true;
            inner.last_receiver_error = None;
            inner.measurements.lock().await.clear();
            let cancellation_token = CancellationToken::new();
            let measurement_task = {
                let address = inner.receiver_address.clone();
                let measurements = inner.measurements.clone();
                let cancellation_token = cancellation_token.clone();
                let tsys_k = inner.tsys_k;
                tokio::task::spawn_blocking(move || {
                    measure(
                        address,
                        measurements,
                        cancellation_token,
                        receiver_configuration,
                        tsys_k,
                    )
                })
            };
            inner.active_integration = Some(ActiveIntegration {
                cancellation_token,
                measurement_task,
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
                match ai.measurement_task.await {
                    Ok(Ok(())) => {}
                    Ok(Err(err)) => error!("Measurement task failed before stop: {err}"),
                    Err(join_err) => {
                        error!("Error waiting for measurement task to finish: {join_err}")
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
            match ai.measurement_task.await {
                Ok(Ok(())) => {}
                Ok(Err(err)) => error!("Measurement task failed before stop: {err}"),
                Err(join_err) => {
                    error!("Error waiting for measurement task to finish: {join_err}")
                }
            }
        }
        // IQ streams do not produce ObservedSpectra. Returning the last entry of
        // `measurements` would surface stale single-dish data from earlier in the
        // process lifetime and mis-file it under the current user/target.
        if kind != Some(IntegrationKind::Spectrum) {
            return None;
        }
        let inner = self.inner.lock().await;
        let measurements = inner.measurements.lock().await;
        measurements.last().map(|m| ObservedSpectra {
            frequencies: m.freqs.clone(),
            spectra: m.amps.clone(),
            observation_time: m.duration,
        })
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
        })
    }
    async fn shutdown(&self) {
        if let Some(tasks) = self.background_tasks.lock().await.take() {
            for task in tasks {
                task.abort();
                let _ = task.await;
            }
        }
        let inner = self.inner.lock().await;
        debug!("Shutting down {}", inner.name);
        inner.controller.shutdown().await;
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
            tokio::task::spawn_blocking(move || {
                measure_iq(address, gpsdo_enabled, token, config, tx)
            })
        };
        inner.receiver_configuration.integrate = true;
        inner.active_integration = Some(ActiveIntegration {
            cancellation_token,
            measurement_task,
            kind: IntegrationKind::Iq,
        });
        Ok(rx)
    }
}

impl Inner {
    async fn update(&mut self, _delta_time: Duration) -> Result<(), TelescopeError> {
        if let Some(active_integration) = self.active_integration.take() {
            if active_integration.measurement_task.is_finished() {
                match active_integration.measurement_task.await {
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
            } else {
                self.active_integration = Some(active_integration);
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

#[allow(clippy::too_many_arguments)]
fn measure_switched(
    usrp: &mut Usrp,
    sfreq: f64,
    rfreq: f64,
    fft_pts: usize,
    tint: f64,
    avg_pts: usize,
    srate: f64,
    rfi_filter: bool,
    tsys: f64,
    cancellation_token: &CancellationToken,
    spec: &mut Vec<f64>,
) -> Result<(), TelescopeError> {
    let mut spec_sig: Vec<f64> = vec![];
    measure_single(
        usrp,
        sfreq,
        fft_pts,
        0.5 * tint,
        avg_pts,
        srate,
        rfi_filter,
        &mut spec_sig,
    )?;
    // Bail between sig and ref if a stop has been requested — otherwise the
    // outer loop's only cancellation check is at the top, and the user waits
    // out the full ~1 s iteration. Leaving `spec` empty signals the outer loop
    // to skip the averaging update and exit cleanly. (Raw mode has no such
    // halfway point, so it still has to wait one full block.)
    if cancellation_token.is_cancelled() {
        return Ok(());
    }
    let mut spec_ref: Vec<f64> = vec![];
    measure_single(
        usrp,
        rfreq,
        fft_pts,
        0.5 * tint,
        avg_pts,
        srate,
        rfi_filter,
        &mut spec_ref,
    )?;
    // Form sig-ref difference and scale with Tsys
    for i in 0..avg_pts {
        spec.push(tsys * (spec_sig[i] - spec_ref[i]) / spec_ref[i]);
    }
    Ok(())
}

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

/// LO offset for direct-conversion tuning (see measure_single): exactly
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

#[allow(clippy::too_many_arguments)]
fn measure_single(
    usrp: &mut Usrp,
    cfreq: f64,
    fft_pts: usize,
    tint: f64,
    avg_pts: usize,
    srate: f64,
    rfi_filter: bool,
    fft_avg: &mut Vec<f64>,
) -> Result<(), TelescopeError> {
    let nsamp: f64 = tint * srate; // total number of samples to request
    let nstack: usize = (nsamp as usize) / fft_pts;
    let navg: usize = fft_pts / avg_pts;

    // The N210 only has one input channel 0. Tune with an LO offset: the
    // DBSRX2 is direct-conversion, so tuning the LO exactly to cfreq puts
    // its leakage/DC artifact in the centre of the band (visible as a
    // narrow dip once the DC-offset correction notches it out). With the
    // offset, the RF LO parks outside the kept passband and the DDC
    // shifts back. See issue #311; requires the wide-open analog filter
    // set at receiver setup.
    usrp.set_rx_frequency(
        &TuneRequest::with_frequency_lo(cfreq, lo_offset_hz(srate)),
        0,
    )
    .map_err(|e| TelescopeError::ReceiverFailed(format!("set_rx_frequency: {e}")))?;

    let mut receiver = usrp
        .get_rx_stream(&uhd::StreamArgs::<Complex<i16>>::new("sc16"))
        .map_err(|e| TelescopeError::ReceiverFailed(format!("get_rx_stream: {e}")))?;

    let mut buffer = vec![Complex::<i16>::default(); nsamp as usize];

    receiver
        .send_command(&StreamCommand {
            command_type: StreamCommandType::CountAndDone(buffer.len() as u64),
            time: StreamTime::Now,
        })
        .map_err(|e| TelescopeError::ReceiverFailed(format!("stream CountAndDone command: {e}")))?;
    receiver
        .receive_simple(buffer.as_mut())
        .map_err(|e| TelescopeError::ReceiverFailed(format!("receive_simple: {e}")))?;

    // Accumulate power spectrum (|FFT|^2) across stacked blocks
    let mut fft_abs: Vec<f64> = Vec::with_capacity(fft_pts);
    fft_abs.resize(fft_pts, 0.0);
    // setup fft
    let mut planner = FftPlanner::new();
    let fft = planner.plan_fft_forward(fft_pts);
    // Loop through the samples, taking fft_pts each time. Samples come from
    // the USRP as i16 (sc16), so we rescale to ±1.0 (fraction of ADC full
    // scale) before the FFT — otherwise |FFT|² lands in the 10⁹+ range purely
    // from the digitiser's count scale, which is not a physical unit.
    const ADC_FULL_SCALE: f64 = 32768.0;
    for n in 0..nstack {
        let mut fft_buffer: Vec<Complex<f64>> = buffer[n * fft_pts..(n + 1) * fft_pts]
            .iter()
            .copied()
            .map(|x| {
                Complex::<f64>::new(x.re as f64 / ADC_FULL_SCALE, x.im as f64 / ADC_FULL_SCALE)
            })
            .collect();
        // Do the FFT
        fft.process(&mut fft_buffer);
        // Accumulate squared magnitudes (Welch's method).
        // Seems the pos/neg halves of spectrum are flipped, so reflip them
        // we want lowest frequency in element 0 and then increasing
        for i in 0..fft_pts / 2 {
            fft_abs[i + fft_pts / 2] += fft_buffer[i].norm_sqr();
            fft_abs[i] += fft_buffer[i + fft_pts / 2].norm_sqr();
        }
    }
    // Average over the stacked blocks
    for val in fft_abs.iter_mut().take(fft_pts) {
        *val /= nstack as f64;
    }

    // Average spectrum to save data
    for i in 0..avg_pts {
        let avg: f64 = fft_abs.iter().skip(navg * i).take(navg).sum();
        fft_avg.push(avg / (navg as f64));
    }

    if rfi_filter {
        clip_rfi_spikes(fft_avg);
    }
    Ok(())
}

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
) -> Result<(), TelescopeError> {
    let tint: f64 = 1.0; // integration time per cycle, seconds
    let srate: f64 = config.bandwidth_hz;
    let sfreq: f64 = config.center_freq_hz;
    let rfreq: f64 = config.ref_freq_hz;
    let avg_pts: usize = config.spectral_channels.max(1);
    let fft_pts: usize = 8192; // ^2 Number of points in FFT, setting spectral resolution
    let gain: f64 = config.gain_db;
    let mode = config.mode;

    // Setup usrp for taking data
    let args = format!("addr={}", address);
    let mut usrp = Usrp::open(&args)
        .map_err(|e| TelescopeError::ReceiverFailed(format!("open USRP at {address}: {e}")))?;

    // The N210 only has one input channel 0. Empty string sets all gains.
    usrp.set_rx_gain(gain, 0, "")
        .map_err(|e| TelescopeError::ReceiverFailed(format!("set_rx_gain: {e}")))?;
    usrp.set_rx_antenna("TX/RX", 0)
        .map_err(|e| TelescopeError::ReceiverFailed(format!("set_rx_antenna: {e}")))?;
    usrp.set_rx_dc_offset_enabled(true, 0)
        .map_err(|e| TelescopeError::ReceiverFailed(format!("set_rx_dc_offset_enabled: {e}")))?;
    usrp.set_rx_sample_rate(srate, 0)
        .map_err(|e| TelescopeError::ReceiverFailed(format!("set_rx_sample_rate: {e}")))?;
    // LO-offset tuning (see measure_single) moves the wanted band away
    // from the analog baseband centre; open the DBSRX2 filter fully so
    // the band sits on the flat part of its response.
    usrp.set_rx_bandwidth(DBSRX2_BANDWIDTH_HZ, 0)
        .map_err(|e| TelescopeError::ReceiverFailed(format!("set_rx_bandwidth: {e}")))?;

    let tsys = tsys_k;
    info!("Tsys = {:.1} K (from config)", tsys);

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

    // start taking data until integrate is false
    let mut n = 0.0;
    while !cancellation_token.is_cancelled() {
        let mut spec = vec![];
        match mode {
            ObservationMode::FreqSwitched => measure_switched(
                &mut usrp,
                sfreq,
                rfreq,
                fft_pts,
                tint,
                avg_pts,
                srate,
                config.rfi_filter,
                tsys,
                &cancellation_token,
                &mut spec,
            )?,
            ObservationMode::Raw => measure_single(
                &mut usrp,
                sfreq,
                fft_pts,
                tint,
                avg_pts,
                srate,
                config.rfi_filter,
                &mut spec,
            )?,
            ObservationMode::Interferometry => break,
        };
        // measure_switched leaves spec empty when it bails on cancellation.
        // Skip the averaging update; the outer while-condition re-checks the
        // token next iteration.
        if spec.is_empty() {
            continue;
        }
        n += 1.0;

        let mut measurements = measurements.blocking_lock();
        let Some(measurement) = measurements.last_mut() else {
            break;
        };
        for (amp, spec_val) in zip(measurement.amps.iter_mut(), spec.iter()).take(avg_pts) {
            *amp = (*amp * (n - 1.0) + spec_val) / n;
        }
        measurement.duration = Utc::now()
            .signed_duration_since(measurement.start)
            .to_std()
            .unwrap();
    }
    Ok(())
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
) -> Result<(), TelescopeError> {
    let args = format!("addr={}", address);
    let mut usrp = Usrp::open(&args).map_err(|e| {
        TelescopeError::ReceiverFailed(format!("IQ stream: open USRP at {address}: {e}"))
    })?;

    if gpsdo_enabled {
        info!("Configuring external clock/PPS sync for {}", address);
        usrp.set_clock_source("external", 0)
            .map_err(|e| TelescopeError::ReceiverFailed(format!("set_clock_source: {e}")))?;
        usrp.set_time_source("external", 0)
            .map_err(|e| TelescopeError::ReceiverFailed(format!("set_time_source: {e}")))?;
        // Latch t=0 on the next PPS edge, then wait for it to settle.
        usrp.set_time_next_pps(0, 0.0, 0)
            .map_err(|e| TelescopeError::ReceiverFailed(format!("set_time_next_pps: {e}")))?;
        std::thread::sleep(Duration::from_secs(2));
        info!("External time sync complete for {}", address);
    }

    usrp.set_rx_gain(config.gain_db, 0, "")
        .map_err(|e| TelescopeError::ReceiverFailed(format!("set_rx_gain: {e}")))?;
    usrp.set_rx_antenna("TX/RX", 0)
        .map_err(|e| TelescopeError::ReceiverFailed(format!("set_rx_antenna: {e}")))?;
    usrp.set_rx_dc_offset_enabled(true, 0)
        .map_err(|e| TelescopeError::ReceiverFailed(format!("set_rx_dc_offset_enabled: {e}")))?;
    usrp.set_rx_sample_rate(config.bandwidth_hz, 0)
        .map_err(|e| TelescopeError::ReceiverFailed(format!("set_rx_sample_rate: {e}")))?;
    // Same LO-offset tuning and wide-open analog filter as the
    // single-dish path (see measure_single): keeps the direct-conversion
    // DC artifact out of the correlated band. Both telescopes in a session
    // get the identical offset, so the inter-device phase relation is
    // unchanged.
    usrp.set_rx_bandwidth(DBSRX2_BANDWIDTH_HZ, 0)
        .map_err(|e| TelescopeError::ReceiverFailed(format!("set_rx_bandwidth: {e}")))?;
    usrp.set_rx_frequency(
        &TuneRequest::with_frequency_lo(config.center_freq_hz, lo_offset_hz(config.bandwidth_hz)),
        0,
    )
    .map_err(|e| TelescopeError::ReceiverFailed(format!("set_rx_frequency: {e}")))?;

    // Open a single continuous stream once and pull IQ_BLOCK_SIZE samples at a time.
    // Re-tuning / reopening the stream per block would invalidate cross-correlation
    // because each setup introduces a fresh, uncorrelated time gap between telescopes.
    let mut receiver = usrp
        .get_rx_stream(&uhd::StreamArgs::<Complex<i16>>::new("sc16"))
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
