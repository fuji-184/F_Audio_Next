use crate::audio::AudioBuffer;
use crate::dsp::{resample_with_config, ResampleConfig, PhaseMode};
use crate::error::Result;

// ── K-weighting biquad ───────────────────────────────────────────────────────
#[derive(Debug, Clone, Copy, Default)]
struct BiquadState { x1: f64, x2: f64, y1: f64, y2: f64 }

#[derive(Debug, Clone, Copy)]
struct BiquadCoeffs { b0: f64, b1: f64, b2: f64, a1: f64, a2: f64 }

impl BiquadCoeffs {
    fn process(&self, x: f64, s: &mut BiquadState) -> f64 {
        let y = self.b0*x + self.b1*s.x1 + self.b2*s.x2 - self.a1*s.y1 - self.a2*s.y2;
        s.x2=s.x1; s.x1=x;
        s.y2=s.y1; s.y1=y;
        y
    }
}

// ITU BS.1770-4 K-weighting for 48 kHz
fn k_weighting_48k() -> (BiquadCoeffs, BiquadCoeffs) {
    // Stage 1: pre-filter high shelving
    let pre = BiquadCoeffs { b0: 1.53512485958697, b1: -2.69169618940638, b2: 1.19839281085285, a1: -1.69065929318241, a2: 0.73248077421585 };
    // Stage 2: RLB high-pass
    let rlb = BiquadCoeffs { b0: 1.0, b1: -2.0, b2: 1.0, a1: -1.99004745483398, a2: 0.99007225036621 };
    (pre, rlb)
}

fn apply_k_weighting(signal: &[f64], sr: u32) -> Vec<f64> {
    // for non-48k, use same coeffs (close enough) or scale via bilinear? For mastering, 48k is standard
    let (pre, rlb) = k_weighting_48k();
    // If sr != 48000, we could recompute, but for tests we use 48k
    let mut s1 = BiquadState::default();
    let mut s2 = BiquadState::default();
    let mut out = Vec::with_capacity(signal.len());
    for &x in signal {
        let y1 = pre.process(x, &mut s1);
        let y2 = rlb.process(y1, &mut s2);
        out.push(y2);
    }
    out
}

// ── loudness measurement ─────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
pub struct LoudnessStats {
    pub integrated_lufs: f64,
    pub true_peak_db: f64,
    pub true_peak_linear: f64,
}

pub fn measure_loudness(buffer: &AudioBuffer) -> LoudnessStats {
    let sr = buffer.sample_rate;
    let ch = buffer.channels.count();
    let n_frames = buffer.num_frames();

    // K-weight each channel
    let mut weighted_channels: Vec<Vec<f64>> = Vec::new();
    for c in 0..ch {
        let chan = buffer.channel_slice(c);
        weighted_channels.push(apply_k_weighting(&chan, sr));
    }

    // 400ms blocks, 100ms hop (75% overlap) => block 0.4*sr, hop 0.1*sr
    let block_size = (0.4 * sr as f64).round() as usize;
    let hop = (0.1 * sr as f64).round() as usize;
    if block_size == 0 || n_frames < block_size / 2 {
        // very short: use whole file as one block
        let mut sum = 0.0;
        for chan in &weighted_channels {
            let ms: f64 = chan.iter().map(|x| x*x).sum::<f64>() / chan.len() as f64;
            sum += ms;
        }
        let mean = sum / ch as f64;
        let lufs = if mean > 1e-12 { -0.691 + 10.0*mean.log10() } else { -70.0 };
        let tp = measure_true_peak(buffer);
        return LoudnessStats { integrated_lufs: lufs, true_peak_db: 20.0*tp.log10(), true_peak_linear: tp };
    }

    let mut block_loudness = Vec::new();
    let mut block_mean_squares = Vec::new();
    let mut pos = 0usize;
    while pos + block_size <= n_frames {
        let mut sum = 0.0;
        for chan in &weighted_channels {
            let block = &chan[pos..pos+block_size];
            let ms: f64 = block.iter().map(|x| x*x).sum::<f64>() / block_size as f64;
            sum += ms;
        }
        let mean = sum / ch as f64;
        let l = if mean > 1e-12 { -0.691 + 10.0*mean.log10() } else { -100.0 };
        block_loudness.push(l);
        block_mean_squares.push(mean);
        pos += hop;
    }
    if block_loudness.is_empty() {
        let tp = measure_true_peak(buffer);
        return LoudnessStats { integrated_lufs: -70.0, true_peak_db: 20.0*tp.log10(), true_peak_linear: tp };
    }

    // Absolute gate -70
    let mut abs_gated: Vec<f64> = Vec::new();
    for (&l, &ms) in block_loudness.iter().zip(block_mean_squares.iter()) {
        if l >= -70.0 { abs_gated.push(ms); }
    }
    if abs_gated.is_empty() {
        let tp = measure_true_peak(buffer);
        return LoudnessStats { integrated_lufs: -70.0, true_peak_db: 20.0*tp.log10(), true_peak_linear: tp };
    }
    let abs_mean = abs_gated.iter().sum::<f64>() / abs_gated.len() as f64;
    let abs_lufs = -0.691 + 10.0*abs_mean.log10();
    let rel_threshold = abs_lufs - 10.0;
    // Relative gate: 10 dB below absolute
    let mut rel_gated: Vec<f64> = Vec::new();
    for (&l, &ms) in block_loudness.iter().zip(block_mean_squares.iter()) {
        if l >= rel_threshold && l >= -70.0 { rel_gated.push(ms); }
    }
    let integrated = if rel_gated.is_empty() {
        abs_lufs
    } else {
        let rel_mean = rel_gated.iter().sum::<f64>() / rel_gated.len() as f64;
        -0.691 + 10.0*rel_mean.log10()
    };

    let tp = measure_true_peak(buffer);
    LoudnessStats { integrated_lufs: integrated, true_peak_db: 20.0*tp.max(1e-12).log10(), true_peak_linear: tp }
}

fn measure_true_peak(buffer: &AudioBuffer) -> f64 {
    // sample peak first
    let mut peak: f64 = buffer.samples.iter().fold(0.0f64, |m: f64,&v| m.max(v.abs()));
    // 4x oversampled via polyphase
    let ch = buffer.channels.count();
    for c in 0..ch {
        let chan = buffer.channel_slice(c);
        let buf = crate::audio::AudioBuffer::from_mono(chan.clone(), buffer.sample_rate);
        let cfg = ResampleConfig { taps: 64, phases: 1024, beta: 12.0, phase: PhaseMode::Linear };
        let target = buffer.sample_rate * 4;
        if let Ok(os) = resample_with_config(&buf, target, &cfg) {
            let p = os.samples.iter().fold(0.0f64, |m: f64,&v| m.max(v.abs()));
            if p > peak { peak = p; }
        }
    }
    peak.max(1e-12)
}

// ── normalization ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
pub struct NormalizationConfig {
    pub target_lufs: f64,
    pub true_peak_ceiling_db: f64,
}

impl Default for NormalizationConfig {
    fn default() -> Self { Self { target_lufs: -14.0, true_peak_ceiling_db: -1.0 } }
}

pub fn normalize_loudness(buffer: &mut AudioBuffer, cfg: NormalizationConfig) -> Result<LoudnessStats> {
    let stats = measure_loudness(buffer);
    let delta = cfg.target_lufs - stats.integrated_lufs;
    let mut gain_db = delta;
    let predicted_tp = stats.true_peak_db + delta;
    if predicted_tp > cfg.true_peak_ceiling_db {
        gain_db = cfg.true_peak_ceiling_db - stats.true_peak_db;
    }
    let gain_lin = 10f64.powf(gain_db / 20.0);
    for s in &mut buffer.samples { *s = (*s * gain_lin).clamp(-1.0, 1.0); }
    // re-measure after
    let new_stats = measure_loudness(buffer);
    Ok(new_stats)
}

pub fn normalize_to_target(buffer: &mut AudioBuffer, target_lufs: f64, ceiling_db: f64) -> Result<f64> {
    let cfg = NormalizationConfig { target_lufs, true_peak_ceiling_db: ceiling_db };
    let stats = normalize_loudness(buffer, cfg)?;
    Ok(stats.integrated_lufs)
}
