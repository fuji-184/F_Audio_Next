use crate::audio::AudioBuffer;
use crate::error::Result;
use std::f64::consts::PI;

// ── config ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct CompressorBandConfig {
    pub low_hz: f64,
    pub high_hz: f64,
    pub threshold_db: f64,
    pub ratio: f64,
    pub attack_ms: f64,
    pub release_ms: f64,
    pub knee_db: f64,
    pub makeup_db: f64,
}

impl CompressorBandConfig {
    pub fn new(low_hz: f64, high_hz: f64, threshold_db: f64, ratio: f64) -> Self {
        Self {
            low_hz,
            high_hz,
            threshold_db,
            ratio: ratio.max(1.0),
            attack_ms: 10.0,
            release_ms: 100.0,
            knee_db: 6.0,
            makeup_db: 0.0,
        }
    }
    pub fn with_attack(mut self, ms: f64) -> Self { self.attack_ms = ms.max(0.1); self }
    pub fn with_release(mut self, ms: f64) -> Self { self.release_ms = ms.max(1.0); self }
    pub fn with_knee(mut self, db: f64) -> Self { self.knee_db = db.max(0.0); self }
    pub fn with_makeup(mut self, db: f64) -> Self { self.makeup_db = db; self }
}

#[derive(Debug, Clone)]
pub struct MultibandCompressorConfig {
    pub bands: Vec<CompressorBandConfig>,
    pub lookahead_ms: f64,
    pub auto_release: bool,
}

impl Default for MultibandCompressorConfig {
    fn default() -> Self {
        Self {
            bands: vec![
                CompressorBandConfig::new(0.0, 250.0, -12.0, 2.0).with_knee(9.0),
                CompressorBandConfig::new(250.0, 4000.0, -10.0, 2.5).with_knee(6.0),
                CompressorBandConfig::new(4000.0, 20000.0, -8.0, 3.0).with_knee(6.0),
            ],
            lookahead_ms: 3.0,
            auto_release: true,
        }
    }
}

impl MultibandCompressorConfig {
    pub fn with_lookahead(mut self, ms: f64) -> Self {
        self.lookahead_ms = ms.clamp(0.0, 5.0);
        self
    }
}

// ── dB helpers ───────────────────────────────────────────────────────────────

#[inline]
fn lin_to_db(x: f64) -> f64 {
    20.0 * x.max(1e-9).log10()
}
#[inline]
fn db_to_lin(db: f64) -> f64 {
    10f64.powf(db / 20.0)
}

// ── windowed sinc FIR ───────────────────────────────────────────────────────

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

fn design_lowpass_fir(cutoff_hz: f64, sample_rate: u32, taps: usize, beta: f64) -> Vec<f64> {
    let n = taps;
    let cutoff = (cutoff_hz / sample_rate as f64).clamp(0.0, 0.499);
    let i0_beta = bessel_i0(beta);
    let mut h = vec![0.0; n];
    let m = (n - 1) as f64 / 2.0;
    for i in 0..n {
        let pos = i as f64 / (n - 1) as f64;
        let w = kaiser_window_pos(pos, beta, i0_beta);
        let x = i as f64 - m;
        let sinc = if x.abs() < 1e-9 { 1.0 } else { (2.0 * PI * cutoff * x).sin() / (2.0 * PI * cutoff * x) };
        h[i] = 2.0 * cutoff * sinc * w;
    }
    let sum: f64 = h.iter().sum();
    if sum.abs() > 1e-12 {
        for v in &mut h {
            *v /= sum;
        }
    }
    h
}

fn design_band_firs(sample_rate: u32, low_hz: f64, high_hz: f64, taps: usize) -> Vec<f64> {
    let nyq = sample_rate as f64 / 2.0;
    let beta = 12.0; // mastering beta for crossover
    if low_hz <= 0.0 && high_hz >= nyq {
        let mut h = vec![0.0; taps];
        h[taps / 2] = 1.0;
        return h;
    }
    if low_hz <= 0.0 {
        return design_lowpass_fir(high_hz, sample_rate, taps, beta);
    }
    if high_hz >= nyq {
        let lp = design_lowpass_fir(low_hz, sample_rate, taps, beta);
        let mut hp = vec![0.0; taps];
        hp[taps / 2] = 1.0;
        for i in 0..taps { hp[i] -= lp[i]; }
        return hp;
    }
    let lp_high = design_lowpass_fir(high_hz, sample_rate, taps, beta);
    let lp_low = design_lowpass_fir(low_hz, sample_rate, taps, beta);
    let mut bp = vec![0.0; taps];
    for i in 0..taps { bp[i] = lp_high[i] - lp_low[i]; }
    bp
}

fn convolve_linear_phase(signal: &[f64], coeffs: &[f64]) -> Vec<f64> {
    let n = signal.len();
    let m = coeffs.len();
    let delay = m / 2;
    let mut out = vec![0.0; n];
    for i in 0..n {
        let mut acc = 0.0;
        for k in 0..m {
            let j = i as isize - k as isize + delay as isize;
            if j >= 0 && j < n as isize {
                acc += signal[j as usize] * coeffs[k];
            }
        }
        out[i] = acc;
    }
    out
}

// ── gain computer ───────────────────────────────────────────────────────────

fn soft_knee_gain_reduction(x_db: f64, threshold: f64, ratio: f64, knee: f64) -> f64 {
    let r = ratio.max(1.0);
    if knee < 0.5 {
        // hard knee
        if x_db <= threshold { 0.0 } else { (x_db - threshold) * (1.0 - 1.0 / r) }
    } else {
        let w = knee;
        if x_db < threshold - w / 2.0 {
            0.0
        } else if x_db > threshold + w / 2.0 {
            (x_db - threshold) * (1.0 - 1.0 / r)
        } else {
            let x = x_db - threshold + w / 2.0;
            x * x * (1.0 - 1.0 / r) / (2.0 * w)
        }
    }
}

// ── per-band processing ─────────────────────────────────────────────────────

fn process_band(
    band_samples: &[f64],
    cfg: &CompressorBandConfig,
    lookahead: usize,
    sample_rate: u32,
    auto_release: bool,
) -> Vec<f64> {
    let n = band_samples.len();
    if n == 0 { return Vec::new(); }

    let sr = sample_rate as f64;
    // lookahead: detector sees future
    let mut delayed = vec![0.0; n];
    for i in 0..n {
        delayed[i] = if i >= lookahead { band_samples[i - lookahead] } else { 0.0 };
    }

    // detector coeffs: RMS window 30-50ms, Peak fast
    let rms_window_ms: f64 = 40.0;
    let rms_alpha = (-1.0 / (rms_window_ms * 0.001 * sr)).exp();
    let attack_coeff = (-1.0 / (cfg.attack_ms * 0.001 * sr)).exp();
    let base_release_coeff = (-1.0 / (cfg.release_ms * 0.001 * sr)).exp();

    let makeup_lin = db_to_lin(cfg.makeup_db);
    // Recompute gains properly with lookahead shift
    let mut gains_db = vec![0.0; n];
    {
        let mut rms = 1e-9;
        let mut peak = 1e-9;
        let mut smooth: f64 = 0.0;
        let mut recent: Vec<f64> = Vec::new();
        for i in 0..n {
            let x = band_samples[i].abs();
            rms = (rms_alpha * rms * rms + (1.0 - rms_alpha) * x * x).sqrt().max(1e-9);
            if x > peak {
                peak = x + attack_coeff * (peak - x);
            } else {
                peak = x + base_release_coeff * (peak - x);
            }
            let det = rms.max(peak).max(1e-9);
            let x_db = lin_to_db(det);
            let crest = lin_to_db(peak) - lin_to_db(rms);
            let rel_ms = if auto_release {
                if crest > 10.0 { 20.0 } else if crest < 4.0 { 400.0 } else { 400.0 - (crest - 4.0) * 63.333 }
            } else { cfg.release_ms };
            let gr = soft_knee_gain_reduction(x_db, cfg.threshold_db, cfg.ratio, cfg.knee_db);
            recent.push(gr);
            if recent.len() > (sr * 0.1) as usize { recent.remove(0); }
            let avg: f64 = recent.iter().sum::<f64>() / recent.len() as f64;
            let rel_eff = if auto_release && avg > 5.0 { (rel_ms * 1.8).min(1000.0) } else { rel_ms };
            let rel_c = (-1.0 / (rel_eff * 0.001 * sr)).exp();
            let target = soft_knee_gain_reduction(x_db, cfg.threshold_db, cfg.ratio, cfg.knee_db);
            let c = if target > smooth { attack_coeff } else { rel_c };
            smooth = target + c * (smooth - target);
            gains_db[i] = smooth;
        }
    }
    // apply gains with lookahead shift: gain at i+lookahead applied to i
    let mut result = vec![0.0; n];
    for i in 0..n {
        let gain_idx = (i + lookahead).min(n - 1);
        let gr_db = gains_db[gain_idx];
        let lin = db_to_lin(-gr_db) * makeup_lin;
        result[i] = delayed[i] * lin;
    }
    result
}

// ── public API ───────────────────────────────────────────────────────────────

pub fn compress_multiband(buffer: &mut AudioBuffer, cfg: &MultibandCompressorConfig) -> Result<()> {
    if buffer.samples.is_empty() || cfg.bands.is_empty() {
        return Ok(());
    }
    let sr = buffer.sample_rate;
    let ch = buffer.channels.count();
    let n_frames = buffer.num_frames();
    let taps = 1024; // linear-phase FIR taps

    // split into bands
    // de-interleave
    let channels: Vec<Vec<f64>> = (0..ch).map(|c| buffer.channel_slice(c)).collect();

    let lookahead = (cfg.lookahead_ms * sr as f64 / 1000.0).round() as usize;

    // design FIRs per band
    let firs: Vec<Vec<f64>> = cfg.bands.iter()
        .map(|b| design_band_firs(sr, b.low_hz, b.high_hz, taps))
        .collect();

    let mut out_channels = vec![vec![0.0; n_frames]; ch];

    for c in 0..ch {
        // split channel into bands
        let mut band_signals: Vec<Vec<f64>> = Vec::new();
        for fir in &firs {
            band_signals.push(convolve_linear_phase(&channels[c], fir));
        }
        // process each band
        let mut band_processed: Vec<Vec<f64>> = Vec::new();
        for (band_sig, band_cfg) in band_signals.iter().zip(cfg.bands.iter()) {
            band_processed.push(process_band(band_sig, band_cfg, lookahead, sr, cfg.auto_release));
        }
        // sum bands
        for i in 0..n_frames {
            let mut sum = 0.0;
            for bp in &band_processed {
                sum += bp[i];
            }
            out_channels[c][i] = sum.clamp(-1.0, 1.0);
        }
    }

    // re-interleave
    let mut out = vec![0.0; n_frames * ch];
    for c in 0..ch {
        for i in 0..n_frames {
            out[i * ch + c] = out_channels[c][i];
        }
    }
    buffer.samples = out;
    Ok(())
}

pub fn compress_singleband(buffer: &mut AudioBuffer, cfg: &CompressorBandConfig, lookahead_ms: f64) -> Result<()> {
    let cfg_mb = MultibandCompressorConfig {
        bands: vec![cfg.clone()],
        lookahead_ms,
        auto_release: true,
    };
    compress_multiband(buffer, &cfg_mb)
}

// ── helpers for testing ─────────────────────────────────────────────────────

pub fn design_crossover_firs_for_test(sample_rate: u32, bands: &[(f64, f64)], taps: usize) -> Vec<Vec<f64>> {
    bands.iter().map(|&(lo, hi)| design_band_firs(sample_rate, lo, hi, taps)).collect()
}
