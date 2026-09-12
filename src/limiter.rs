use crate::audio::AudioBuffer;
use crate::dsp::{resample_with_config, ResampleConfig, PhaseMode};
use crate::error::Result;

// ── config ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LimiterStyle {
    Aggressive,   // fast release 20ms, preserves loudness at cost of pumping
    Transparent,  // slow release 300ms, smooth
    Punchy,       // medium 80ms, balances
}

#[derive(Debug, Clone, Copy)]
pub struct LimiterConfig {
    pub ceiling_db: f64,
    pub lookahead_ms: f64,
    pub oversample_factor: usize,
    pub style: LimiterStyle,
}

impl Default for LimiterConfig {
    fn default() -> Self {
        Self { ceiling_db: -1.0, lookahead_ms: 2.0, oversample_factor: 4, style: LimiterStyle::Transparent }
    }
}
impl LimiterConfig {
    pub fn with_ceiling(mut self, db: f64) -> Self { self.ceiling_db = db.clamp(-12.0, 0.0); self }
    pub fn with_lookahead(mut self, ms: f64) -> Self { self.lookahead_ms = ms.clamp(0.0, 5.0); self }
    pub fn with_style(mut self, s: LimiterStyle) -> Self { self.style=s; self }
}

// ── helpers ──────────────────────────────────────────────────────────────────

fn db_to_lin(db: f64) -> f64 { 10f64.powf(db/20.0) }

fn resample_vec(data:&[f64], from:u32, to:u32)->Vec<f64>{
    let buf=AudioBuffer::from_mono(data.to_vec(), from);
    let cfg=ResampleConfig{ phase: PhaseMode::Linear, ..Default::default() };
    resample_with_config(&buf,to,&cfg).unwrap().samples
}

fn tpdf_dither(samples: &mut [f64], bits: u32) {
    // TPDF: sum of two uniform
    let lsb = 1.0 / ((1u32 << bits) as f64);
    let mut seed: u64 = 0x12345678;
    for s in samples.iter_mut() {
        let r1 = {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            (seed>>33) as f64 / (1u64<<31) as f64
        };
        let r2 = {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            (seed>>33) as f64 / (1u64<<31) as f64
        };
        let tpdf = (r1 - 0.5 + r2 - 0.5) * lsb;
        *s = (*s + tpdf).clamp(-1.0,1.0);
    }
}

// ── core ─────────────────────────────────────────────────────────────────────

pub fn apply_limiter(buffer: &mut AudioBuffer, cfg: &LimiterConfig) -> Result<()> {
    if buffer.samples.is_empty() { return Ok(()); }
    let sr = buffer.sample_rate;
    let ch = buffer.channels.count();
    let n_frames = buffer.num_frames();
    let os_factor = cfg.oversample_factor.max(1);
    let sr_os = sr * os_factor as u32;
    let ceiling_lin = db_to_lin(cfg.ceiling_db);
    let lookahead_os = (cfg.lookahead_ms * sr_os as f64 / 1000.0).round() as usize;

    // de-interleave
    let channels: Vec<Vec<f64>> = (0..ch).map(|c| buffer.channel_slice(c)).collect();

    // oversample each channel
    let mut os_channels: Vec<Vec<f64>> = Vec::new();
    for chan in &channels {
        let os = resample_vec(chan, sr, sr_os);
        os_channels.push(os);
    }
    let os_len = os_channels[0].len();

    // lookahead max envelope: A(n) = max_{k=0..D} |x_os(n+k)| / ceiling
    let d = lookahead_os.max(1);
    let mut raw_gain = vec![1.0; os_len];
    // For stereo linked, compute max across channels
    for n in 0..os_len {
        let mut max_val: f64 = 0.0;
        for chan_os in &os_channels {
            let end = (n + d).min(os_len);
            for k in n..end {
                let v = chan_os[k].abs();
                if v > max_val { max_val = v; }
            }
        }
        let needed = max_val / ceiling_lin;
        let a = if needed > 1.0 { needed } else { 1.0 };
        raw_gain[n] = 1.0 / a; // <=1
    }

    // parallel envelope trackers: fast and slow
    let (fast_rel_ms, slow_rel_ms) = match cfg.style {
        LimiterStyle::Aggressive => (20.0, 80.0),
        LimiterStyle::Transparent => (150.0, 400.0),
        LimiterStyle::Punchy => (40.0, 200.0),
    };
    let att_a = 0.0; // instantaneous brickwall attack
    let fast_a = (-1.0/(fast_rel_ms*0.001*sr_os as f64)).exp();
    let slow_a = (-1.0/(slow_rel_ms*0.001*sr_os as f64)).exp();

    // Two envelopes
    let mut fast_env: f64 = 1.0;
    let mut slow_env: f64 = 1.0;
    let mut smoothed = vec![1.0; os_len];
    for n in 0..os_len {
        let target = raw_gain[n];
        // fast
        let fa = if target < fast_env { att_a } else { fast_a };
        fast_env = target + fa*(fast_env - target);
        // slow
        let sa = if target < slow_env { att_a } else { slow_a };
        slow_env = target + sa*(slow_env - target);
        // style blend
        let g = match cfg.style {
            LimiterStyle::Aggressive => fast_env,
            LimiterStyle::Transparent => slow_env,
            LimiterStyle::Punchy => 0.6*fast_env + 0.4*slow_env,
        };
        smoothed[n] = g;
    }

    // apply gain to oversampled channels
    let mut os_out: Vec<Vec<f64>> = Vec::new();
    for chan_os in &os_channels {
        let mut y = Vec::with_capacity(os_len);
        for n in 0..os_len {
            y.push((chan_os[n] * smoothed[n]).clamp(-1.0,1.0));
        }
        os_out.push(y);
    }

    // downsample back to original rate (linear-phase de-aliasing)
    let mut out_channels: Vec<Vec<f64>> = Vec::new();
    for y_os in &os_out {
        let down = resample_vec(y_os, sr_os, sr);
        // trim/pad to n_frames
        let mut v = vec![0.0; n_frames];
        let copy = n_frames.min(down.len());
        for i in 0..copy { v[i]=down[i]; }
        // dithering for 16-bit (optional, but we add subtle TPDF at -90dB)
        // We do not dither unless requested; keep f64 precise
        // For mastering, we could add TPDF at 16-bit LSB
        // Here we add very low level dither to avoid quantization distortion
        // tpdf_dither(&mut v, 16);
        out_channels.push(v);
    }

    // re-interleave
    let mut out = vec![0.0; n_frames*ch];
    for c in 0..ch {
        for i in 0..n_frames {
            out[i*ch + c]= out_channels[c][i].clamp(-1.0,1.0);
        }
    }
    buffer.samples = out;
    Ok(())
}

// For true-peak measurement helper (used in normalization)
pub fn true_peak_after_limiter(buffer: &AudioBuffer, cfg: &LimiterConfig) -> f64 {
    let mut tmp = buffer.clone();
    let _ = apply_limiter(&mut tmp, cfg);
    tmp.samples.iter().fold(0.0f64, |m,&v| m.max(v.abs()))
}
