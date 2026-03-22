use crate::coords::{Direction, Location};
use crate::models::telescope::Telescope;
use crate::models::telescope_types::{
    Measurement, ObservationMode, ObservedSpectra, ReceiverConfiguration, ReceiverError,
    TelescopeError, TelescopeInfo, TelescopeTarget,
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

pub struct ActiveIntegration {
    cancellation_token: CancellationToken,
    measurement_task: tokio::task::JoinHandle<()>,
}

struct Inner {
    name: String,
    receiver_address: String,
    controller: TelescopeTracker,
    receiver_configuration: ReceiverConfiguration,
    measurements: Arc<Mutex<Vec<Measurement>>>,
    active_integration: Option<ActiveIntegration>,
    stow_position: Option<Direction>,
    location: Location,
    min_elevation_rad: f64,
    t_rec_k: f64,
    receiver_reachable: Arc<tokio::sync::Mutex<bool>>,
}

pub struct SalsaTelescope {
    inner: Arc<Mutex<Inner>>,
}

#[allow(clippy::too_many_arguments)]
pub fn create(
    name: String,
    controller_address: String,
    receiver_address: String,
    stow_position: Option<Direction>,
    location: Location,
    min_elevation_rad: f64,
    default_ref_freq_hz: f64,
    default_gain_db: f64,
    t_rec_k: f64,
    tle_cache: TleCacheHandle,
) -> SalsaTelescope {
    let receiver_reachable = Arc::new(tokio::sync::Mutex::new(false));
    let ping_reachable = receiver_reachable.clone();
    let ping_address = receiver_address.clone();

    let inner = Arc::new(Mutex::new(Inner {
        name,
        receiver_address,
        controller: TelescopeTracker::new(
            controller_address,
            location,
            min_elevation_rad,
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
        stow_position,
        location,
        min_elevation_rad,
        t_rec_k,
        receiver_reachable,
    }));

    let task_inner = inner.clone();
    tokio::spawn(async move {
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

    tokio::spawn(async move {
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
            *ping_reachable.lock().await = reachable;
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    });

    SalsaTelescope { inner }
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
            inner.measurements.lock().await.clear();
            let cancellation_token = CancellationToken::new();
            let measurement_task = {
                let address = inner.receiver_address.clone();
                let measurements = inner.measurements.clone();
                let cancellation_token = cancellation_token.clone();
                let t_rec_k = inner.t_rec_k;
                tokio::spawn(async move {
                    measure(
                        address,
                        measurements,
                        cancellation_token,
                        receiver_configuration,
                        t_rec_k,
                    )
                    .await;
                })
            };
            inner.active_integration = Some(ActiveIntegration {
                cancellation_token,
                measurement_task,
            });
        } else if !receiver_configuration.integrate && inner.receiver_configuration.integrate {
            info!("Stopping integration");
            if let Some(active_integration) = &mut inner.active_integration {
                active_integration.cancellation_token.cancel();
            }
            inner.receiver_configuration.integrate = false;
        }
        Ok(inner.receiver_configuration)
    }

    async fn get_info(&self) -> Result<TelescopeInfo, TelescopeError> {
        let inner = self.inner.lock().await;
        let receiver_reachable = *inner.receiver_reachable.lock().await;
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
            most_recent_error: controller_info.most_recent_error,
            measurement_in_progress: inner.active_integration.is_some(),
            latest_observation,
            stow_position: inner.stow_position,
            az_offset_rad: controller_info.az_offset_rad,
            el_offset_rad: controller_info.el_offset_rad,
            location: inner.location,
            min_elevation_rad: inner.min_elevation_rad,
            receiver_reachable: Some(receiver_reachable),
        })
    }
    async fn shutdown(&self) {
        let inner = self.inner.lock().await;
        debug!("Shutting down {}", inner.name);
        inner.controller.shutdown().await;
    }
}

impl Inner {
    async fn update(&mut self, _delta_time: Duration) -> Result<(), TelescopeError> {
        if let Some(active_integration) = self.active_integration.take() {
            if active_integration.measurement_task.is_finished() {
                if let Err(error) = active_integration.measurement_task.await {
                    error!("Error while waiting for measurement task: {}", error);
                }
            } else {
                self.active_integration = Some(active_integration);
            }
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

async fn fetch_ambient_temp_k() -> f64 {
    let url = "https://www.oso.chalmers.se/weather/onsala.txt";
    let result: Option<f64> = async {
        let text = reqwest::get(url).await.ok()?.text().await.ok()?;
        let mut parts = text.split_whitespace();
        let timestamp: i64 = parts.next()?.parse().ok()?;
        let temp_celsius: f64 = parts.next()?.parse().ok()?;
        let age_secs = chrono::Utc::now().timestamp() - timestamp;
        if age_secs > 120 || !(-30.0..=50.0).contains(&temp_celsius) {
            return None;
        }
        Some(temp_celsius + 273.15)
    }
    .await;
    match result {
        Some(t) => {
            info!("Ambient temperature: {:.1} K", t);
            t
        }
        None => {
            warn!(
                "Could not fetch ambient temperature, using fallback {} K",
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

    let tsys = fetch_ambient_temp_k().await + t_rec_k;
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
