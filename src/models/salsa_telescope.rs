use crate::coords::Direction;
use crate::models::telescope::Telescope;
use crate::models::telescope_types::{
    Measurement, ObservedSpectra, ReceiverConfiguration, ReceiverError, TelescopeError,
    TelescopeInfo, TelescopeTarget,
};
use crate::telescope_tracker::TelescopeTracker;
use async_trait::async_trait;
use chrono::Utc;
use std::iter::zip;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info};

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
}

pub struct SalsaTelescope {
    inner: Arc<Mutex<Inner>>,
}

pub fn create(
    name: String,
    controller_address: String,
    receiver_address: String,
) -> SalsaTelescope {
    let inner = Arc::new(Mutex::new(Inner {
        name,
        receiver_address,
        controller: TelescopeTracker::new(controller_address),
        receiver_configuration: ReceiverConfiguration { integrate: false },
        measurements: Arc::new(Mutex::new(Vec::new())),
        active_integration: None,
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

    SalsaTelescope { inner }
}

#[async_trait]
impl Telescope for SalsaTelescope {
    async fn get_direction(&self) -> Option<Direction> {
        let inner = self.inner.lock().await;
        inner.controller.direction()
    }

    async fn set_target(&self, target: TelescopeTarget) -> Result<TelescopeTarget, TelescopeError> {
        let mut inner = self.inner.lock().await;
        inner.controller.set_target(target)
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
            let cancellation_token = CancellationToken::new();
            let measurement_task = {
                let address = inner.receiver_address.clone();
                let measurements = inner.measurements.clone();
                let cancellation_token = cancellation_token.clone();
                tokio::spawn(async move {
                    measure(address, measurements, cancellation_token).await;
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
    spec: &mut [f64],
) {
    let mut spec_sig: Vec<f64> = vec![];
    measure_single(
        usrp,
        sfreq,
        fft_pts,
        0.5 * tint,
        avg_pts,
        srate,
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
        &mut spec_ref,
    );
    // Form sig-ref difference and scale with Tsys
    // Hard coded Tsys for now
    let tsys = 285.0;
    for i in 0..avg_pts {
        spec[i] = tsys * (spec_sig[i] - spec_ref[i]) / spec_ref[i];
    }
}

fn measure_single(
    usrp: &mut Usrp,
    cfreq: f64,
    fft_pts: usize,
    tint: f64,
    avg_pts: usize,
    srate: f64,
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

    // Average spectrum to save data
    for i in 0..avg_pts {
        let avg: f64 = fft_abs.iter().skip(navg * i).take(navg).sum();
        fft_avg.push(avg / (navg as f64));
    }
}

fn median(mut xs: Vec<f64>) -> f64 {
    // sort in ascending order, panic on f64::NaN
    xs.sort_by(|x, y| x.partial_cmp(y).unwrap());
    let n = xs.len();
    if n.is_multiple_of(2) {
        (xs[n / 2] + xs[n / 2 - 1]) / 2.0
    } else {
        xs[n / 2]
    }
}

async fn measure(
    address: String,
    measurements: Arc<Mutex<Vec<Measurement>>>,
    cancellation_token: CancellationToken,
) {
    // Switched HI example
    let tint: f64 = 1.0; // integration time per cycle, seconds
    let srate: f64 = 2.5e6; // sample rate, Hz
    let sfreq: f64 = 1.4204e9;
    let rfreq: f64 = 1.4179e9;
    let avg_pts: usize = 512; // ^2 Number of points after average, setting spectral resolution
    let fft_pts: usize = 8192; // ^2 Number of points in FFT, setting spectral resolution
    let gain: f64 = 60.0;

    // Setup usrp for taking data
    let args = format!("addr={}", address);
    let mut usrp = Usrp::open(&args).unwrap(); // Brage

    // The N210 only has one input channel 0.
    usrp.set_rx_gain(gain, 0, "").unwrap(); // empty string to set all gains
    usrp.set_rx_antenna("TX/RX", 0).unwrap();
    usrp.set_rx_dc_offset_enabled(true, 0).unwrap();

    usrp.set_rx_sample_rate(srate, 0).unwrap();

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
        let mut spec = vec![0.0; avg_pts];
        measure_switched(
            &mut usrp, sfreq, rfreq, fft_pts, tint, avg_pts, srate, &mut spec,
        );
        n += 1.0;

        let mut measurements = measurements.lock().await;
        let measurement = measurements.last_mut().unwrap();
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
