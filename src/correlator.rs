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
use std::f64::consts::PI;
use std::sync::Arc;

use chrono::Utc;
use rusqlite::Connection;
use rustfft::{FftPlanner, num_complex::Complex};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use crate::models::interferometry::InterferometryVisibility;
use crate::models::telescope_types::{IQ_BLOCK_SIZE, IqBlock, ReceiverConfiguration};

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
        rx_a: tokio::sync::mpsc::Receiver<IqBlock>,
        rx_b: tokio::sync::mpsc::Receiver<IqBlock>,
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
    mut rx_a: tokio::sync::mpsc::Receiver<IqBlock>,
    mut rx_b: tokio::sync::mpsc::Receiver<IqBlock>,
    config: ReceiverConfiguration,
    db: Arc<Mutex<Connection>>,
    token: CancellationToken,
) {
    let samples_per_second = config.bandwidth_hz as usize;
    let spectral_channels = config.spectral_channels.clamp(1, IQ_BLOCK_SIZE);
    let bins_per_channel = (IQ_BLOCK_SIZE / spectral_channels).max(1);

    // Two blocks are "aligned" if their timestamps differ by less than half a block.
    // Any larger gap means the two receivers drifted (dropped packets, slipped PPS,
    // etc.) and their samples no longer correspond — cross-correlating them would
    // produce scientifically meaningless output.
    let block_duration_secs = IQ_BLOCK_SIZE as f64 / config.bandwidth_hz;
    let align_tolerance_secs = block_duration_secs * 0.5;

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
    let freqs_json = serde_json::to_string(&freqs).expect("serializing Vec<f64> never fails");

    // Buffered look-ahead of one block per side, so we can discard a stale block
    // from the earlier side without losing a fresh block from the other.
    let mut pending_a: Option<IqBlock> = None;
    let mut pending_b: Option<IqBlock> = None;
    loop {
        if pending_a.is_none() {
            pending_a = tokio::select! {
                r = rx_a.recv() => match r { Some(b) => Some(b), None => break },
                _ = token.cancelled() => break,
            };
        }
        if pending_b.is_none() {
            pending_b = tokio::select! {
                r = rx_b.recv() => match r { Some(b) => Some(b), None => break },
                _ = token.cancelled() => break,
            };
        }

        let (Some(block_a), Some(block_b)) = (pending_a.as_ref(), pending_b.as_ref()) else {
            // Unreachable — both slots are populated by the selects above, and both
            // selects exit the loop via `break` when the channel closes or the
            // cancellation token fires.
            break;
        };
        let delta = block_a.timestamp_secs - block_b.timestamp_secs;

        if delta.abs() > align_tolerance_secs {
            // The two sides are out of alignment. Drop the earlier block and try
            // again — this also resets the 1-second accumulator because any
            // partial integration we've built was computed from misaligned blocks.
            warn!(
                "Correlator: A/B timestamp delta {:.3}s exceeds {:.3}s tolerance — dropping {} block and resetting accumulator",
                delta,
                align_tolerance_secs,
                if delta < 0.0 { "A" } else { "B" }
            );
            if delta < 0.0 {
                pending_a = None;
            } else {
                pending_b = None;
            }
            acc.fill(Complex::new(0.0, 0.0));
            num_blocks = 0;
            samples_acc = 0;
            continue;
        }

        // Take ownership for the FFT step.
        let block_a = pending_a.take().expect("populated above");
        let block_b = pending_b.take().expect("populated above");

        // FFT both blocks
        let mut fa: Vec<Complex<f64>> = block_a
            .samples
            .iter()
            .map(|x| Complex::new(x.re as f64, x.im as f64))
            .collect();
        let mut fb: Vec<Complex<f64>> = block_b
            .samples
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

        // Normalise by number of accumulated blocks. The absolute scale of the
        // resulting amplitudes is arbitrary (no FFT-size normalisation, no
        // auto-correlation divide for coherence), so `mean_amplitude` is only
        // meaningful relative to other integrations in the same session.
        let norm = num_blocks as f64;
        let v_norm: Vec<Complex<f64>> = acc.iter().map(|x| x / norm).collect();

        // Coarse delay from IFFT peak. Bin spacing is 1/bandwidth_hz, so the
        // peak bin pins the delay to within ±½ bin — fringe_fit then refines
        // it sub-bin by fitting the phase ramp across the band.
        let mut v_delay = v_norm.clone();
        fft_inverse.process(&mut v_delay);
        let peak_idx = v_delay
            .iter()
            .enumerate()
            .max_by(|(_, a), (_, b)| {
                a.norm()
                    .partial_cmp(&b.norm())
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|(i, _)| i)
            .unwrap_or(0);
        let tau_coarse_s = if peak_idx < IQ_BLOCK_SIZE / 2 {
            peak_idx as f64 / config.bandwidth_hz
        } else {
            (peak_idx as i64 - IQ_BLOCK_SIZE as i64) as f64 / config.bandwidth_hz
        };

        // fftshift: rearrange so lowest frequency is at index 0
        let v_shifted: Vec<Complex<f64>> = (0..IQ_BLOCK_SIZE)
            .map(|i| v_norm[(i + IQ_BLOCK_SIZE / 2) % IQ_BLOCK_SIZE])
            .collect();

        // Bin into spectral_channels output channels (keep complex form for
        // the fringe fit; expose amp/phase per channel for the UI plots).
        let mut bin_complex = vec![Complex::<f64>::new(0.0, 0.0); spectral_channels];
        let mut bin_amps = vec![0.0f64; spectral_channels];
        let mut bin_phases = vec![0.0f64; spectral_channels];
        for ch in 0..spectral_channels {
            let mut sum = Complex::<f64>::new(0.0, 0.0);
            for b in 0..bins_per_channel {
                sum += v_shifted[ch * bins_per_channel + b];
            }
            let mean_vis = sum / bins_per_channel as f64;
            bin_complex[ch] = mean_vis;
            bin_amps[ch] = mean_vis.norm();
            bin_phases[ch] = mean_vis.arg().to_degrees();
        }

        // Fringe fit: refine delay and band-centre phase from the per-channel
        // cross-power. Replaces the bin-quantised IFFT peak readout — at
        // SNR ~100 over the band, σ_τ ≈ √3/(π·B·SNR), i.e. ~6 ns at 1 MHz.
        let f_off: Vec<f64> = freqs.iter().map(|f| f - config.center_freq_hz).collect();
        let (tau_refined_s, phi_center_rad) = fringe_fit(&bin_complex, &f_off, tau_coarse_s);
        let delay_ns = tau_refined_s * 1e9;

        // Coherent-sum amplitude after de-rotation by τ_refined. Equivalent
        // to the old circular mean when τ_refined ≈ 0, but doesn't suppress
        // the magnitude when there's an actual delay across the band.
        let mean_vis: Complex<f64> = bin_complex
            .iter()
            .zip(f_off.iter())
            .map(|(v, &f)| v * Complex::from_polar(1.0, 2.0 * PI * f * tau_refined_s))
            .sum::<Complex<f64>>()
            / spectral_channels as f64;
        let mean_amplitude = mean_vis.norm();
        let mean_phase_deg = phi_center_rad.to_degrees();

        let amps_json = serde_json::to_string(&bin_amps).expect("serializing Vec<f64> never fails");
        let phases_json =
            serde_json::to_string(&bin_phases).expect("serializing Vec<f64> never fails");

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

/// Refine the cross-correlation delay and band-centre phase by maximising the
/// coherent sum |Σ V[ch]·exp(+i·2π·f_off[ch]·τ)| in τ. `tau_coarse_s` should
/// be within ±½ IFFT bin (≤ 1/(2·B)) of the true delay — the IFFT peak
/// satisfies this by construction.
///
/// The sign convention assumes V[ch] = M·exp(-i·2π·f_off[ch]·τ + iφ₀) + noise,
/// matching the rustfft forward convention used upstream (a positive cross-
/// delay produces a negative phase ramp across the band).
///
/// Each iteration: de-rotate by the current τ, take the coherent sum's phase
/// as the bulk phase φ₀, subtract it from per-channel phases so residuals
/// cluster near zero (no unwrap), then weighted-LSQ-fit the residual slope.
/// Three iterations suffice from a ½-bin starting point.
///
/// Returns (refined delay in seconds, phase at band centre in radians).
fn fringe_fit(v_chan: &[Complex<f64>], f_off: &[f64], tau_coarse_s: f64) -> (f64, f64) {
    let two_pi = 2.0 * PI;
    let mut tau = tau_coarse_s;
    let mut phi_center = 0.0;

    for _ in 0..3 {
        let v_rot: Vec<Complex<f64>> = v_chan
            .iter()
            .zip(f_off.iter())
            .map(|(v, &f)| v * Complex::from_polar(1.0, two_pi * f * tau))
            .collect();

        let s0: Complex<f64> = v_rot.iter().sum();
        if s0.norm() <= 0.0 {
            return (tau, phi_center);
        }
        phi_center = s0.arg();
        let bulk_rot = Complex::from_polar(1.0, -phi_center);

        let phi: Vec<f64> = v_rot.iter().map(|v| (v * bulk_rot).arg()).collect();
        let w: Vec<f64> = v_rot.iter().map(|v| v.norm_sqr()).collect();

        let mut sw = 0.0;
        let mut swx = 0.0;
        let mut swxx = 0.0;
        let mut swy = 0.0;
        let mut swxy = 0.0;
        for ((wi, &x), &y) in w.iter().zip(f_off.iter()).zip(phi.iter()) {
            sw += wi;
            swx += wi * x;
            swxx += wi * x * x;
            swy += wi * y;
            swxy += wi * x * y;
        }
        let denom = sw * swxx - swx * swx;
        if denom.abs() < f64::EPSILON {
            return (tau, phi_center);
        }
        let slope = (sw * swxy - swx * swy) / denom; // rad / Hz
        // Residual ramp -2π·Δτ·f_off matches a fitted slope b ⇒ Δτ = -b/(2π).
        let delta_tau = -slope / two_pi;
        tau += delta_tau;

        if delta_tau.abs() < 1e-15 {
            break;
        }
    }
    (tau, phi_center)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;
    use rand::rngs::StdRng;
    use rand_distr::{Distribution, Normal};

    /// Build N per-channel cross-power visibilities for a band of width
    /// `bw_hz` centred on baseband zero, carrying a known delay and phase.
    fn synth_visibilities(
        n: usize,
        bw_hz: f64,
        tau_s: f64,
        phi_rad: f64,
        amplitude: f64,
        noise_sigma: f64,
        seed: u64,
    ) -> (Vec<Complex<f64>>, Vec<f64>) {
        let f_off: Vec<f64> = (0..n)
            .map(|ch| -bw_hz / 2.0 + (ch as f64 + 0.5) * bw_hz / n as f64)
            .collect();
        let mut rng = StdRng::seed_from_u64(seed);
        let noise = Normal::new(0.0, noise_sigma).unwrap();
        let v_chan: Vec<Complex<f64>> = f_off
            .iter()
            .map(|&f| {
                let signal = Complex::from_polar(amplitude, -2.0 * PI * f * tau_s + phi_rad);
                signal + Complex::new(noise.sample(&mut rng), noise.sample(&mut rng))
            })
            .collect();
        (v_chan, f_off)
    }

    fn wrap_pi(x: f64) -> f64 {
        let two_pi = 2.0 * PI;
        ((x + PI).rem_euclid(two_pi)) - PI
    }

    /// Inject a 100 ns sub-sample delay at 2.5 MHz BW (½ bin = 200 ns) with a
    /// noise-free signal — recovery should be effectively exact.
    #[test]
    fn fringe_fit_recovers_100ns_at_2_5mhz_clean() {
        let (v, f_off) = synth_visibilities(256, 2.5e6, 100e-9, 0.3, 1.0, 0.0, 1);
        let (tau_s, phi) = fringe_fit(&v, &f_off, 0.0);
        assert!(
            (tau_s * 1e9 - 100.0).abs() < 0.01,
            "delay: got {} ns expected 100 ns",
            tau_s * 1e9
        );
        assert!(
            wrap_pi(phi - 0.3).abs() < 1e-9,
            "phase: got {} rad expected 0.3 rad",
            phi
        );
    }

    /// Same at 1 MHz, where the ½-bin window is 500 ns.
    #[test]
    fn fringe_fit_recovers_100ns_at_1mhz_clean() {
        let (v, f_off) = synth_visibilities(256, 1.0e6, 100e-9, -1.2, 1.0, 0.0, 2);
        let (tau_s, phi) = fringe_fit(&v, &f_off, 0.0);
        assert!((tau_s * 1e9 - 100.0).abs() < 0.01);
        assert!(wrap_pi(phi - (-1.2)).abs() < 1e-9);
    }

    /// IFFT peak can be off by up to ½ bin — start the fit there and confirm
    /// it still converges to the truth. At 2.5 MHz BW one bin = 400 ns; we
    /// inject 350 ns and seed the coarse delay at the next bin (400 ns).
    #[test]
    fn fringe_fit_converges_from_half_bin_off() {
        let (v, f_off) = synth_visibilities(256, 2.5e6, 350e-9, 0.0, 1.0, 0.0, 3);
        let (tau_s, _) = fringe_fit(&v, &f_off, 400e-9);
        assert!(
            (tau_s * 1e9 - 350.0).abs() < 0.01,
            "got {} ns expected 350 ns",
            tau_s * 1e9
        );
    }

    /// With Gaussian noise, the delay scatter should match the textbook
    /// formula σ_τ ≈ √3 / (π·B·ρ). Run many seeds and verify the empirical
    /// RMS sits within a generous factor of 2 of the prediction.
    #[test]
    fn fringe_fit_noise_precision_matches_textbook() {
        let n = 256;
        let bw = 1.0e6;
        let amplitude = 1.0;
        let noise_sigma = 0.1; // per-channel σ on each (re, im) component
        // Per-channel SNR (amplitude/noise) = 10; total coherent SNR over the
        // band is amplitude·√n / σ ≈ 160. σ_τ_pred ≈ √3 / (π · B · SNR_total).
        let snr_total = amplitude * (n as f64).sqrt() / noise_sigma;
        let sigma_tau_pred = 3f64.sqrt() / (PI * bw * snr_total);

        let trials = 200;
        let mut sum_sq = 0.0;
        for seed in 0..trials {
            let (v, f_off) = synth_visibilities(n, bw, 0.0, 0.0, amplitude, noise_sigma, seed);
            let (tau_s, _) = fringe_fit(&v, &f_off, 0.0);
            sum_sq += tau_s * tau_s;
        }
        let sigma_tau_empirical = (sum_sq / trials as f64).sqrt();
        assert!(
            sigma_tau_empirical < 2.0 * sigma_tau_pred,
            "σ_τ empirical {:.2e} s vs predicted {:.2e} s",
            sigma_tau_empirical,
            sigma_tau_pred
        );
        assert!(
            sigma_tau_empirical > 0.3 * sigma_tau_pred,
            "σ_τ empirical {:.2e} s suspiciously low vs predicted {:.2e} s",
            sigma_tau_empirical,
            sigma_tau_pred
        );
    }
}
