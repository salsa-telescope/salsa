/// FX correlator for two-element interferometry.
///
/// Receives raw IQ blocks from two telescopes, computes cross-power spectra,
/// accumulates for 1 second, then stores a Visibility row to the database.
///
/// Algorithm per 1-second integration:
///   1. Receive matching blocks (Vec<Complex<f32>>, IQ_BLOCK_SIZE samples each)
///   2. FFT each block → S_a[f], S_b[f]
///   3. Accumulate cross-power: V[f] += S_a[f] * conj(S_b[f])
///   4. After bandwidth_hz samples accumulated (= 1 second):
///      - Apply fftshift so channels are in increasing-frequency order
///      - Bin into spectral_channels output channels
///      - Compute delay spectrum via IFFT(V), find peak → delay_ns
///      - Store to DB
use std::sync::Arc;

use chrono::Utc;
use rusqlite::Connection;
use rustfft::{FftPlanner, num_complex::Complex};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use tracing::{error, info};

use crate::models::interferometry::InterferometryVisibility;
use crate::models::salsa_telescope::IQ_BLOCK_SIZE;
use crate::models::telescope_types::ReceiverConfiguration;

pub struct CorrelatorHandle {
    pub session_id: i64,
    pub telescope_a: String,
    pub telescope_b: String,
    cancellation_token: CancellationToken,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl CorrelatorHandle {
    pub fn start(
        session_id: i64,
        telescope_a: String,
        telescope_b: String,
        rx_a: tokio::sync::mpsc::Receiver<Vec<Complex<f32>>>,
        rx_b: tokio::sync::mpsc::Receiver<Vec<Complex<f32>>>,
        config: ReceiverConfiguration,
        db: Arc<Mutex<Connection>>,
    ) -> Self {
        let token = CancellationToken::new();
        let task = tokio::spawn(correlator_task(
            session_id,
            rx_a,
            rx_b,
            config,
            db,
            token.clone(),
        ));
        info!(
            "Correlator started for session {} ({} × {})",
            session_id, telescope_a, telescope_b
        );
        Self {
            session_id,
            telescope_a,
            telescope_b,
            cancellation_token: token,
            task: Some(task),
        }
    }

    pub async fn stop(&mut self) {
        self.cancellation_token.cancel();
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
        info!("Correlator stopped for session {}", self.session_id);
    }
}

async fn correlator_task(
    session_id: i64,
    mut rx_a: tokio::sync::mpsc::Receiver<Vec<Complex<f32>>>,
    mut rx_b: tokio::sync::mpsc::Receiver<Vec<Complex<f32>>>,
    config: ReceiverConfiguration,
    db: Arc<Mutex<Connection>>,
    token: CancellationToken,
) {
    let samples_per_second = config.bandwidth_hz as usize;
    let spectral_channels = config.spectral_channels.clamp(1, IQ_BLOCK_SIZE);
    let bins_per_channel = (IQ_BLOCK_SIZE / spectral_channels).max(1);

    let mut planner = FftPlanner::<f64>::new();
    let fft_forward = planner.plan_fft_forward(IQ_BLOCK_SIZE);
    let fft_inverse = planner.plan_fft_inverse(IQ_BLOCK_SIZE);

    // Accumulated cross-power spectrum in FFT-natural order (DC at index 0)
    let mut acc = vec![Complex::<f64>::new(0.0, 0.0); IQ_BLOCK_SIZE];
    let mut num_blocks: usize = 0;
    let mut samples_acc: usize = 0;

    // Frequency axis for output channels (increasing order after fftshift)
    let freqs: Vec<f64> = (0..spectral_channels)
        .map(|ch| {
            config.center_freq_hz - config.bandwidth_hz / 2.0
                + (ch as f64 + 0.5) * config.bandwidth_hz / spectral_channels as f64
        })
        .collect();
    let freqs_json = serde_json::to_string(&freqs).unwrap_or_default();

    loop {
        // Receive one block from each telescope, exit on cancellation or channel close
        let block_a = tokio::select! {
            r = rx_a.recv() => match r { Some(b) => b, None => break },
            _ = token.cancelled() => break,
        };
        let block_b = tokio::select! {
            r = rx_b.recv() => match r { Some(b) => b, None => break },
            _ = token.cancelled() => break,
        };

        // FFT both blocks
        let mut fa: Vec<Complex<f64>> = block_a
            .iter()
            .map(|x| Complex::new(x.re as f64, x.im as f64))
            .collect();
        let mut fb: Vec<Complex<f64>> = block_b
            .iter()
            .map(|x| Complex::new(x.re as f64, x.im as f64))
            .collect();
        fft_forward.process(&mut fa);
        fft_forward.process(&mut fb);

        // Accumulate cross-power: V[f] += A[f] * conj(B[f])
        for i in 0..IQ_BLOCK_SIZE {
            acc[i] += fa[i] * fb[i].conj();
        }
        num_blocks += 1;
        samples_acc += IQ_BLOCK_SIZE;

        if samples_acc < samples_per_second {
            continue;
        }

        // --- 1-second integration complete: compute and store visibility ---

        // Normalise by number of accumulated blocks
        let norm = num_blocks as f64;
        let v_norm: Vec<Complex<f64>> = acc.iter().map(|x| x / norm).collect();

        // Delay spectrum: IFFT of cross-power (FFT-natural order, no shift needed)
        let mut v_delay = v_norm.clone();
        fft_inverse.process(&mut v_delay);
        let delay_amps: Vec<f64> = v_delay.iter().map(|x| x.norm()).collect();
        let peak_idx = delay_amps
            .iter()
            .enumerate()
            .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(i, _)| i)
            .unwrap_or(0);
        let delay_ns = if peak_idx < IQ_BLOCK_SIZE / 2 {
            peak_idx as f64 / config.bandwidth_hz * 1e9
        } else {
            (peak_idx as i64 - IQ_BLOCK_SIZE as i64) as f64 / config.bandwidth_hz * 1e9
        };

        // fftshift: rearrange so lowest frequency is at index 0
        let v_shifted: Vec<Complex<f64>> = (0..IQ_BLOCK_SIZE)
            .map(|i| v_norm[(i + IQ_BLOCK_SIZE / 2) % IQ_BLOCK_SIZE])
            .collect();

        // Bin into spectral_channels output channels
        let mut bin_amps = vec![0.0f64; spectral_channels];
        let mut bin_phases = vec![0.0f64; spectral_channels];
        for ch in 0..spectral_channels {
            let mut sum_amp = 0.0f64;
            let mut sum_phase = 0.0f64;
            for b in 0..bins_per_channel {
                let idx = ch * bins_per_channel + b;
                sum_amp += v_shifted[idx].norm();
                sum_phase += v_shifted[idx].arg().to_degrees();
            }
            bin_amps[ch] = sum_amp / bins_per_channel as f64;
            bin_phases[ch] = sum_phase / bins_per_channel as f64;
        }

        let mean_amplitude = bin_amps.iter().sum::<f64>() / spectral_channels as f64;
        let mean_phase_deg = bin_phases.iter().sum::<f64>() / spectral_channels as f64;

        let amps_json = serde_json::to_string(&bin_amps).unwrap_or_default();
        let phases_json = serde_json::to_string(&bin_phases).unwrap_or_default();

        if let Err(e) = InterferometryVisibility::insert(
            db.clone(),
            session_id,
            Utc::now(),
            mean_amplitude,
            mean_phase_deg,
            delay_ns,
            amps_json,
            phases_json,
            freqs_json.clone(),
        )
        .await
        {
            error!("Correlator: failed to insert visibility: {e:?}");
        }

        // Reset accumulator
        acc.fill(Complex::new(0.0, 0.0));
        num_blocks = 0;
        samples_acc = 0;
    }
}
