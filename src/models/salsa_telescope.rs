use crate::coords::{Direction, Location};
use crate::models::telescope::Telescope;
use crate::models::telescope_types::{
    IQ_BLOCK_SIZE, IqBlock, Measurement, ObservationMode, ObservedSpectra, ReceiverConfiguration,
    ReceiverError, TelescopeError, TelescopeInfo, TelescopeTarget,
};
use crate::telescope_tracker::TelescopeTracker;
use crate::tle_cache::TleCacheHandle;
use crate::weather_cache::WeatherCacheHandle;
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
    measurement_task: tokio::task::JoinHandle<()>,
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
    t_rec_k: f64,
    wind_warning_ms: Option<f64>,
    weather_cache: WeatherCacheHandle,
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
    t_rec_k: f64,
    wind_warning_ms: Option<f64>,
    tle_cache: TleCacheHandle,
    weather_cache: WeatherCacheHandle,
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
        t_rec_k,
        wind_warning_ms,
        weather_cache,
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

    async fn set_receiver_configuration(
        &self,
        receiver_configuration: ReceiverConfiguration,
    ) -> Result<ReceiverConfiguration, ReceiverError> {
        let mut inner = self.inner.lock().await;
        if receiver_configuration.integrate && !inner.receiver_configuration.integrate {
            if inner.active_integration.is_some() {
                return Err(ReceiverError::IntegrationAlreadyRunning);
            }

            info!("Starting integration");
            inner.receiver_configuration.integrate = true;
            inner.last_receiver_error = None;
            inner.measurements.lock().await.clear();
            let cancellation_token = CancellationToken::new();
            let measurement_task = {
                let address = inner.receiver_address.clone();
                let measurements = inner.measurements.clone();
                let cancellation_token = cancellation_token.clone();
                let t_rec_k = inner.t_rec_k;
                let weather_cache = inner.weather_cache.clone();
                tokio::spawn(async move {
                    measure(
                        address,
                        measurements,
                        cancellation_token,
                        receiver_configuration,
                        t_rec_k,
                        weather_cache,
                    )
                    .await;
                })
            };
            inner.active_integration = Some(ActiveIntegration {
                cancellation_token,
                measurement_task,
                kind: IntegrationKind::Spectrum,
            });
        } else if !receiver_configuration.integrate && inner.receiver_configuration.integrate {
            info!("Stopping integration");
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
                if let Err(err) = ai.measurement_task.await {
                    error!("Error waiting for measurement task to finish: {err}");
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
            inner.receiver_configuration.integrate = false;
            inner.active_integration.take()
        };
        // Lock is dropped — safe to await the task without risk of deadlock.
        let kind = active_integration.as_ref().map(|ai| ai.kind);
        if let Some(ai) = active_integration {
            ai.cancellation_token.cancel();
            if let Err(err) = ai.measurement_task.await {
                error!("Error waiting for measurement task to finish: {err}");
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
            tokio::spawn(async move {
                measure_iq(address, gpsdo_enabled, token, config, tx).await;
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
                if let Err(error) = active_integration.measurement_task.await {
                    error!("Error while waiting for measurement task: {}", error);
                    self.last_receiver_error =
                        Some(TelescopeError::ReceiverFailed(error.to_string()));
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
    spec: &mut Vec<f64>,
) {
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
    );
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
    );
    // Form sig-ref difference and scale with Tsys
    for i in 0..avg_pts {
        spec.push(tsys * (spec_sig[i] - spec_ref[i]) / spec_ref[i]);
    }
}

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
) {
    let nsamp: f64 = tint * srate; // total number of samples to request
    let nstack: usize = (nsamp as usize) / fft_pts;
    let navg: usize = fft_pts / avg_pts;

    usrp.set_rx_frequency(&TuneRequest::with_frequency(cfreq), 0)
        .unwrap(); // The N210 only has one input channel 0.

    let mut receiver = usrp
        .get_rx_stream(&uhd::StreamArgs::<Complex<i16>>::new("sc16"))
        .unwrap();

    let mut buffer = vec![Complex::<i16>::default(); nsamp as usize];

    receiver
        .send_command(&StreamCommand {
            command_type: StreamCommandType::CountAndDone(buffer.len() as u64),
            time: StreamTime::Now,
        })
        .unwrap();
    receiver.receive_simple(buffer.as_mut()).unwrap();

    // array to store power spectrum (abs of FFT result)
    let mut fft_abs: Vec<f64> = Vec::with_capacity(fft_pts);
    fft_abs.resize(fft_pts, 0.0);
    // setup fft
    let mut planner = FftPlanner::new();
    let fft = planner.plan_fft_forward(fft_pts);
    // Loop through the samples, taking fft_pts each time
    for n in 0..nstack {
        let mut fft_buffer: Vec<Complex<f64>> = buffer[n * fft_pts..(n + 1) * fft_pts]
            .iter()
            .copied()
            .map(|x| Complex::<f64>::new(x.re as f64, x.im as f64))
            .collect();
        // Do the FFT
        fft.process(&mut fft_buffer);
        // Add absolute values to stacked spectrum
        // Seems the pos/neg halves of spectrum are flipped, so reflip them
        // we want lowest frequency in element 0 and then increasing
        for i in 0..fft_pts / 2 {
            fft_abs[i + fft_pts / 2] += fft_buffer[i].norm();
            fft_abs[i] += fft_buffer[i + fft_pts / 2].norm();
        }
    }
    // Normalise spectrum by number of stackings,
    // do **2 to get power spectrum
    for val in fft_abs.iter_mut().take(fft_pts) {
        *val = *val * *val / (nstack as f64);
    }

    // median window filter data
    if rfi_filter {
        let mwkernel = 32; //median window filter size, power of 2
        let threshold = 0.1; // threshold where to cut data and replace with median
        let nchunks = fft_pts / mwkernel;
        for i in 0..nchunks {
            let chunk = &mut fft_abs[i * mwkernel..(i + 1) * mwkernel];
            let m = median(chunk.to_vec());
            for val in chunk.iter_mut() {
                let diff = (*val - m).abs();
                if diff > threshold * m {
                    *val = m;
                }
            }
        }
    }

    // Average spectrum to save data
    for i in 0..avg_pts {
        let avg: f64 = fft_abs.iter().skip(navg * i).take(navg).sum();
        fft_avg.push(avg / (navg as f64));
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

const AMBIENT_TEMP_FALLBACK_K: f64 = 285.0;

fn ambient_temp_k_from_cache(weather_cache: &WeatherCacheHandle) -> f64 {
    let result = weather_cache.get().and_then(|w| {
        if w.age_secs() > 120 || !(-30.0..=50.0).contains(&w.temp_c) {
            return None;
        }
        Some(w.temp_c + 273.15)
    });
    match result {
        Some(t) => {
            info!("Ambient temperature: {:.1} K", t);
            t
        }
        None => {
            warn!(
                "Could not get ambient temperature from cache, using fallback {} K",
                AMBIENT_TEMP_FALLBACK_K
            );
            AMBIENT_TEMP_FALLBACK_K
        }
    }
}

async fn measure(
    address: String,
    measurements: Arc<Mutex<Vec<Measurement>>>,
    cancellation_token: CancellationToken,
    config: ReceiverConfiguration,
    t_rec_k: f64,
    weather_cache: WeatherCacheHandle,
) {
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
    let mut usrp = Usrp::open(&args).unwrap(); // Brage

    // The N210 only has one input channel 0.
    usrp.set_rx_gain(gain, 0, "").unwrap(); // empty string to set all gains
    usrp.set_rx_antenna("TX/RX", 0).unwrap();
    usrp.set_rx_dc_offset_enabled(true, 0).unwrap();

    usrp.set_rx_sample_rate(srate, 0).unwrap();

    let tsys = ambient_temp_k_from_cache(&weather_cache) + t_rec_k;
    info!("Tsys = {:.1} K (T_rec = {:.1} K)", tsys, t_rec_k);

    {
        let mut measurements = measurements.clone().lock_owned().await;
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
                &mut spec,
            ),
            ObservationMode::Raw => measure_single(
                &mut usrp,
                sfreq,
                fft_pts,
                tint,
                avg_pts,
                srate,
                config.rfi_filter,
                &mut spec,
            ),
            ObservationMode::Interferometry => break,
        };
        n += 1.0;

        let mut measurements = measurements.lock().await;
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
}

/// Stream raw IQ blocks for interferometry cross-correlation.
/// Sends blocks of IQ_BLOCK_SIZE Complex<f32> samples over `tx`.
/// Exits when `cancellation_token` fires or the receiver drops the channel.
async fn measure_iq(
    address: String,
    gpsdo_enabled: bool,
    cancellation_token: CancellationToken,
    config: ReceiverConfiguration,
    tx: tokio::sync::mpsc::Sender<IqBlock>,
) {
    let args = format!("addr={}", address);
    let mut usrp = match Usrp::open(&args) {
        Ok(u) => u,
        Err(e) => {
            error!("IQ stream: failed to open USRP at {}: {}", address, e);
            return;
        }
    };

    if gpsdo_enabled {
        info!("Configuring external clock/PPS sync for {}", address);
        if let Err(e) = usrp.set_clock_source("external", 0) {
            error!("set_clock_source failed: {}", e);
            return;
        }
        if let Err(e) = usrp.set_time_source("external", 0) {
            error!("set_time_source failed: {}", e);
            return;
        }
        // Latch t=0 on the next PPS edge, then wait for it to settle.
        if let Err(e) = usrp.set_time_next_pps(0, 0.0, 0) {
            error!("set_time_next_pps failed: {}", e);
            return;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
        info!("External time sync complete for {}", address);
    }

    usrp.set_rx_gain(config.gain_db, 0, "").unwrap();
    usrp.set_rx_antenna("TX/RX", 0).unwrap();
    usrp.set_rx_dc_offset_enabled(true, 0).unwrap();
    usrp.set_rx_sample_rate(config.bandwidth_hz, 0).unwrap();
    if let Err(e) = usrp.set_rx_frequency(&TuneRequest::with_frequency(config.center_freq_hz), 0) {
        error!("IQ stream: set_rx_frequency failed: {}", e);
        return;
    }

    // Open a single continuous stream once and pull IQ_BLOCK_SIZE samples at a time.
    // Re-tuning / reopening the stream per block would invalidate cross-correlation
    // because each setup introduces a fresh, uncorrelated time gap between telescopes.
    let mut receiver = match usrp.get_rx_stream(&uhd::StreamArgs::<Complex<i16>>::new("sc16")) {
        Ok(r) => r,
        Err(e) => {
            error!("IQ stream: get_rx_stream failed: {}", e);
            return;
        }
    };
    if let Err(e) = receiver.send_command(&StreamCommand {
        command_type: StreamCommandType::StartContinuous,
        time: StreamTime::Now,
    }) {
        error!("IQ stream: StartContinuous failed: {}", e);
        return;
    }

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
            .send(IqBlock {
                timestamp_secs,
                samples,
            })
            .await
            .is_err()
        {
            break;
        }
    }

    if let Err(e) = receiver.send_command(&StreamCommand {
        command_type: StreamCommandType::StopContinuous,
        time: StreamTime::Now,
    }) {
        error!("IQ stream: StopContinuous failed: {}", e);
    }
}

#[cfg(test)]
mod test {
    use super::*;

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
