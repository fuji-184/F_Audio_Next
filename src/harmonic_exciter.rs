use crate::audio::AudioBuffer;
use crate::dsp::{resample_with_config, ResampleConfig, PhaseMode};
use crate::error::Result;
use std::f64::consts::PI;

// ── config ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ExciterMode {
    Even,
    Odd,
    Tape,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SidechainMode {
    All,
    Transient,
    Sustained,
}

#[derive(Debug, Clone)]
pub struct ExciterBandConfig {
    pub low_hz: f64,
    pub high_hz: f64,
    pub drive: f64,      // α for Even, β for Tape, gain for Odd
    pub gamma: f64,      // for Tape
    pub mix: f64,        // 0..1 wet
    pub mode: ExciterMode,
    pub sidechain: SidechainMode,
}

impl ExciterBandConfig {
    pub fn new(low_hz: f64, high_hz: f64, mode: ExciterMode) -> Self {
        Self {
            low_hz,
            high_hz,
            drive: 0.02,
            gamma: 2.0,
            mix: 0.3,
            mode,
            sidechain: SidechainMode::All,
        }
    }
    pub fn with_drive(mut self, d: f64) -> Self { self.drive = d; self }
    pub fn with_gamma(mut self, g: f64) -> Self { self.gamma = g; self }
    pub fn with_mix(mut self, m: f64) -> Self { self.mix = m.clamp(0.0, 1.0); self }
    pub fn with_sidechain(mut self, s: SidechainMode) -> Self { self.sidechain = s; self }
}

#[derive(Debug, Clone)]
pub struct HarmonicExciterConfig {
    pub bands: Vec<ExciterBandConfig>,
    pub oversample_factor: usize,
}

impl Default for HarmonicExciterConfig {
    fn default() -> Self {
        Self {
            bands: vec![
                ExciterBandConfig::new(2000.0, 8000.0, ExciterMode::Even).with_drive(0.02).with_mix(0.25),
                ExciterBandConfig::new(8000.0, 20000.0, ExciterMode::Tape).with_drive(0.8).with_gamma(1.8).with_mix(0.2),
            ],
            oversample_factor: 4,
        }
    }
}

// ── helpers ──────────────────────────────────────────────────────────────────

fn bessel_i0(x: f64) -> f64 {
    let mut sum = 1.0;
    let mut term = 1.0;
    let mut k = 1usize;
    let xh = x * 0.5;
    let xh2 = xh * xh;
    loop {
        term *= xh2 / (k as f64 * k as f64);
        sum += term;
        if term < 1e-16 * sum || k > 100 { break; }
        k += 1;
    }
    sum
}
fn kaiser_window_pos(pos: f64, beta: f64, i0_beta: f64) -> f64 {
    let r = 2.0 * pos - 1.0;
    let arg = beta * (1.0 - r * r).max(0.0).sqrt();
    bessel_i0(arg) / i0_beta
}
fn design_lowpass_fir(cutoff_hz: f64, sr: u32, taps: usize, beta: f64) -> Vec<f64> {
    let cutoff = (cutoff_hz / sr as f64).clamp(0.0, 0.499);
    let i0_beta = bessel_i0(beta);
    let mut h = vec![0.0; taps];
    let m = (taps - 1) as f64 / 2.0;
    for i in 0..taps {
        let pos = i as f64 / (taps - 1) as f64;
        let w = kaiser_window_pos(pos, beta, i0_beta);
        let x = i as f64 - m;
        let sinc = if x.abs() < 1e-9 { 1.0 } else { (2.0 * PI * cutoff * x).sin() / (2.0 * PI * cutoff * x) };
        h[i] = 2.0 * cutoff * sinc * w;
    }
    let sum: f64 = h.iter().sum();
    if sum.abs() > 1e-12 { for v in &mut h { *v /= sum; } }
    h
}
fn design_band_firs(sr: u32, low_hz: f64, high_hz: f64, taps: usize) -> Vec<f64> {
    let nyq = sr as f64 / 2.0;
    let beta = 12.0;
    if low_hz <= 0.0 && high_hz >= nyq { let mut h = vec![0.0; taps]; h[taps/2]=1.0; return h; }
    if low_hz <= 0.0 { return design_lowpass_fir(high_hz, sr, taps, beta); }
    if high_hz >= nyq {
        let lp = design_lowpass_fir(low_hz, sr, taps, beta);
        let mut hp = vec![0.0; taps]; hp[taps/2]=1.0;
        for i in 0..taps { hp[i]-=lp[i]; }
        return hp;
    }
    let lp_h = design_lowpass_fir(high_hz, sr, taps, beta);
    let lp_l = design_lowpass_fir(low_hz, sr, taps, beta);
    let mut bp = vec![0.0; taps];
    for i in 0..taps { bp[i]=lp_h[i]-lp_l[i]; }
    bp
}
fn convolve_linear_phase(signal: &[f64], coeffs: &[f64]) -> Vec<f64> {
    let n = signal.len();
    let m = coeffs.len();
    let delay = m/2;
    let mut out = vec![0.0; n];
    for i in 0..n {
        let mut acc = 0.0;
        for k in 0..m {
            let j = i as isize - k as isize + delay as isize;
            if j>=0 && j < n as isize { acc+= signal[j as usize]*coeffs[k]; }
        }
        out[i]=acc;
    }
    out
}

fn resample_vec(data: &[f64], from: u32, to: u32) -> Vec<f64> {
    let buf = AudioBuffer::from_mono(data.to_vec(), from);
    let cfg = ResampleConfig { phase: PhaseMode::Linear, ..Default::default() };
    resample_with_config(&buf, to, &cfg).unwrap().samples
}

// ── wave-shapers ─────────────────────────────────────────────────────────────

fn waveshape_even(x: f64, alpha: f64) -> f64 {
    // f(x)= x + α·x² — pure even harmonic, soft-clip to prevent runaway
    let y = x + alpha * x * x;
    // soft-knee: tanh-like but preserve asymmetry
    if y > 1.0 { 1.0 - (-(y - 1.0)).exp() * 0.1 } else if y < -1.0 { -1.0 + (-( -y - 1.0)).exp() * 0.1 } else { y }
}
fn waveshape_odd(x: f64, drive: f64) -> f64 {
    (x * drive).tanh()
}
fn waveshape_tape(x: f64, beta: f64, gamma: f64) -> f64 {
    let denom = (1.0 + beta * x.abs()).powf(1.0 / gamma.max(0.1));
    x / denom
}

// ── envelope follower for sidechain ─────────────────────────────────────────

fn envelope_follower(signal: &[f64], sr: u32, fast_ms: f64, slow_ms: f64) -> (Vec<f64>, Vec<f64>) {
    let fast_a = (-1.0 / (fast_ms * 0.001 * sr as f64)).exp();
    let slow_a = (-1.0 / (slow_ms * 0.001 * sr as f64)).exp();
    let mut fast = 0.0;
    let mut slow = 0.0;
    let mut fast_env = Vec::with_capacity(signal.len());
    let mut slow_env = Vec::with_capacity(signal.len());
    for &x in signal {
        let ax = x.abs();
        fast = ax + fast_a * (fast - ax);
        slow = ax + slow_a * (slow - ax);
        fast_env.push(fast);
        slow_env.push(slow);
    }
    (fast_env, slow_env)
}

fn sidechain_weights(signal: &[f64], sr: u32, mode: SidechainMode) -> Vec<f64> {
    if matches!(mode, SidechainMode::All) {
        return vec![1.0; signal.len()];
    }
    let (fast, slow) = envelope_follower(signal, sr, 1.0, 50.0);
    let mut w = Vec::with_capacity(signal.len());
    for i in 0..signal.len() {
        let f = fast[i].max(1e-9);
        let s = slow[i].max(1e-9);
        let weight = match mode {
            SidechainMode::Transient => ((f - s) / f).clamp(0.0, 1.0),
            SidechainMode::Sustained => (s / f).clamp(0.0, 1.0),
            SidechainMode::All => 1.0,
        };
        w.push(weight);
    }
    w
}

// ── per-band process ─────────────────────────────────────────────────────────

fn process_band_channel(
    band: &[f64],
    sr: u32,
    cfg: &ExciterBandConfig,
    oversample: usize,
) -> Vec<f64> {
    let n = band.len();
    // sidechain weights at original rate
    let weights = sidechain_weights(band, sr, cfg.sidechain);

    // oversample band
    let sr_os = sr * oversample as u32;
    let band_os = resample_vec(band, sr, sr_os);
    // weights oversampled via linear interpolation (resample as well)
    let weights_os = {
        let w_buf = AudioBuffer::from_mono(weights.clone(), sr);
        let cfg_rs = ResampleConfig { phase: PhaseMode::Linear, ..Default::default() };
        resample_with_config(&w_buf, sr_os, &cfg_rs).unwrap().samples
    };

    // wave-shaping on OS domain
    let mut shaped_os = Vec::with_capacity(band_os.len());
    for (i, &x) in band_os.iter().enumerate() {
        let w = weights_os[i.min(weights_os.len() - 1)];
        // drive scaled by sidechain
        let y = match cfg.mode {
            ExciterMode::Even => {
                let alpha = cfg.drive.clamp(0.001, 0.05);
                let dry = x;
                let shaped = waveshape_even(x, alpha);
                // harmonics only, scaled by sidechain
                dry + (shaped - dry) * w
            }
            ExciterMode::Odd => {
                let drive = cfg.drive.clamp(0.5, 10.0);
                let shaped = waveshape_odd(x, drive);
                let diff = shaped - x * (1.0 / drive.max(1.0));
                x + diff * w
            }
            ExciterMode::Tape => {
                let beta = cfg.drive.clamp(0.1, 5.0);
                let gamma = cfg.gamma.clamp(0.5, 5.0);
                let shaped = waveshape_tape(x, beta, gamma);
                let diff = shaped - x;
                x + diff * w
            }
        };
        shaped_os.push(y.clamp(-1.2, 1.2));
    }

    // downsample with brick-wall (linear-phase)
    let shaped = resample_vec(&shaped_os, sr_os, sr);
    // ensure same length as input (resample may have rounding)
    let mut out_harm = vec![0.0; n];
    let copy_len = n.min(shaped.len());
    // harmonics = shaped - band (the added part), high-pass to remove DC from even
    // For mastering, we want only harmonics above band low, so simple high-pass via removing DC
    // Compute harmonics as shaped - band, then trim low-freq DC
    for i in 0..copy_len {
        let harm = shaped[i] - band[i];
        out_harm[i] = harm;
    }
    // simple DC blocker (one-pole highpass at 20 Hz) to remove even-harmonic DC
    if matches!(cfg.mode, ExciterMode::Even) {
        let rc = 1.0 / (2.0 * PI * 20.0);
        let dt = 1.0 / sr as f64;
        let alpha = rc / (rc + dt);
        let mut prev_x = 0.0;
        let mut prev_y = 0.0;
        let mut y;
        for i in 0..n {
            let x = out_harm[i];
            y = alpha * (prev_y + x - prev_x);
            prev_x = x;
            prev_y = y;
            out_harm[i] = y;
        }
    }
    // pad if needed
    out_harm.truncate(n);
    // apply mix: wet/dry parallel
    // band contains original, harm is added saturation
    // final = band + harm * mix
    let mut result = vec![0.0; n];
    for i in 0..n {
        result[i] = band[i] + out_harm[i] * cfg.mix;
    }
    result
}

// ── public API ───────────────────────────────────────────────────────────────

pub fn apply_harmonic_exciter(buffer: &mut AudioBuffer, cfg: &HarmonicExciterConfig) -> Result<()> {
    if buffer.samples.is_empty() || cfg.bands.is_empty() {
        return Ok(());
    }
    let sr = buffer.sample_rate;
    let ch = buffer.channels.count();
    let n_frames = buffer.num_frames();
    let taps = 1024;

    let channels: Vec<Vec<f64>> = (0..ch).map(|c| buffer.channel_slice(c)).collect();
    let firs: Vec<Vec<f64>> = cfg.bands.iter()
        .map(|b| design_band_firs(sr, b.low_hz, b.high_hz, taps))
        .collect();

    let mut out_channels = vec![vec![0.0; n_frames]; ch];

    for c in 0..ch {
        // split into bands
        let mut band_sigs: Vec<Vec<f64>> = Vec::new();
        for fir in &firs {
            band_sigs.push(convolve_linear_phase(&channels[c], fir));
        }
        // also need residual (outside bands) to preserve untouched spectrum
        // compute sum of band FIRs to find residual
        let mut sum_fir = vec![0.0; taps];
        for fir in &firs { for i in 0..taps { sum_fir[i]+=fir[i]; } }
        let mut residual = channels[c].clone();
        // residual = original - sum(bands) but sum already accounts via convolution
        // Instead compute residual as original - sum(bands) after convolution
        let sum_bands: Vec<f64> = {
            let mut s = vec![0.0; n_frames];
            for bs in &band_sigs { for i in 0..n_frames { s[i]+=bs[i]; } }
            s
        };
        for i in 0..n_frames { residual[i] -= sum_bands[i]; }

        // process each band
        let mut processed_bands: Vec<Vec<f64>> = Vec::new();
        for (band_sig, band_cfg) in band_sigs.iter().zip(cfg.bands.iter()) {
            processed_bands.push(process_band_channel(band_sig, sr, band_cfg, cfg.oversample_factor));
        }
        // sum processed bands + residual (untouched)
        for i in 0..n_frames {
            let mut sum = residual[i];
            for pb in &processed_bands { sum+=pb[i]; }
            out_channels[c][i]=sum.clamp(-1.0, 1.0);
        }
    }

    let mut out = vec![0.0; n_frames*ch];
    for c in 0..ch {
        for i in 0..n_frames { out[i*ch+c]=out_channels[c][i]; }
    }
    buffer.samples = out;
    Ok(())
}

// for stereo-linked sidechain, we could share weights across channels
pub fn apply_harmonic_exciter_stereo_linked(buffer: &mut AudioBuffer, cfg: &HarmonicExciterConfig) -> Result<()> {
    // For now, linked is implicit via identical processing per channel with same config
    // True linked would compute envelope from max of L/R, but our per-channel envelope is similar for correlated material
    apply_harmonic_exciter(buffer, cfg)
}
