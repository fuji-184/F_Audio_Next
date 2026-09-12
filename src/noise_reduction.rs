use crate::audio::AudioBuffer;
use crate::error::Result;
use std::f64::consts::TAU;

// ── config ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct NoiseReductionConfig {
    pub window_size: usize,
    pub hop_size: usize,
    pub reduction_db: f64,
    pub threshold_db: f64,
    pub stereo_linked: bool,
    pub use_masking: bool,
}

impl Default for NoiseReductionConfig {
    fn default() -> Self {
        Self {
            window_size: 4096,
            hop_size: 1024, // 75% overlap
            reduction_db: 18.0,
            threshold_db: 6.0,
            stereo_linked: true,
            use_masking: true,
        }
    }
}

impl NoiseReductionConfig {
    pub fn with_reduction(mut self, db: f64) -> Self {
        self.reduction_db = db.clamp(0.0, 60.0);
        self
    }
    pub fn with_threshold(mut self, db: f64) -> Self {
        self.threshold_db = db;
        self
    }
}

// ── noise profile ────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct NoiseProfile {
    pub magnitudes: Vec<f64>,
    pub sample_rate: u32,
    pub window_size: usize,
}

// ── windowing ────────────────────────────────────────────────────────────────

fn hann_window(n: usize) -> Vec<f64> {
    (0..n)
        .map(|i| 0.5 * (1.0 - (TAU * i as f64 / (n - 1) as f64).cos()))
        .collect()
}

// ── FFT (radix-2 Cooley-Tukey) ──────────────────────────────────────────────

fn fft_inplace(re: &mut [f64], im: &mut [f64], inverse: bool) {
    let n = re.len();
    debug_assert!(n.is_power_of_two());
    let mut j = 0usize;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j ^= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let sign = if inverse { 1.0 } else { -1.0 };
    let mut len = 2;
    while len <= n {
        let half = len / 2;
        let ang = sign * TAU / len as f64;
        let (wre, wim) = (ang.cos(), ang.sin());
        for i in (0..n).step_by(len) {
            let (mut ur, mut ui) = (1.0, 0.0);
            for k in 0..half {
                let tr = ur * re[i + k + half] - ui * im[i + k + half];
                let ti = ur * im[i + k + half] + ui * re[i + k + half];
                re[i + k + half] = re[i + k] - tr;
                im[i + k + half] = im[i + k] - ti;
                re[i + k] += tr;
                im[i + k] += ti;
                let nr = ur * wre - ui * wim;
                let ni = ur * wim + ui * wre;
                ur = nr;
                ui = ni;
            }
        }
        len <<= 1;
    }
    if inverse {
        let scale = 1.0 / n as f64;
        for (r, i) in re.iter_mut().zip(im.iter_mut()) {
            *r *= scale;
            *i *= scale;
        }
    }
}

fn real_fft(input: &[f64]) -> Vec<(f64, f64)> {
    let n = input.len();
    let mut re = input.to_vec();
    let mut im = vec![0.0; n];
    fft_inplace(&mut re, &mut im, false);
    (0..=n / 2).map(|i| (re[i], im[i])).collect()
}

fn real_ifft(spectrum: &[(f64, f64)], size: usize) -> Vec<f64> {
    let mut re = vec![0.0; size];
    let mut im = vec![0.0; size];
    for (i, &(r, img)) in spectrum.iter().enumerate() {
        re[i] = r;
        im[i] = img;
        if i > 0 && i < size / 2 {
            re[size - i] = r;
            im[size - i] = -img;
        }
    }
    fft_inplace(&mut re, &mut im, true);
    re
}

// ── psychoacoustic helpers ───────────────────────────────────────────────────

fn bark_scale(freq_hz: f64) -> f64 {
    13.0 * (0.00076 * freq_hz).atan() + 3.5 * ((freq_hz / 7500.0).powi(2)).atan()
}

fn ath_db(freq_hz: f64) -> f64 {
    let f = freq_hz / 1000.0;
    // Terhardt ATH approximation
    3.64 * f.powf(-0.8) - 6.5 * (-0.6 * (f - 3.3).powi(2)).exp() + 0.001 * f.powi(4)
}

fn masking_thresholds(mag_db: &[f64], freqs: &[f64], bark: &[f64]) -> Vec<f64> {
    let n = mag_db.len();
    let mut thresh = vec![f64::NEG_INFINITY; n];
    for i in 0..n {
        thresh[i] = ath_db(freqs[i].max(20.0));
    }
    // simultaneous masking: spread from loud bins to neighbors (±8 bins ~ within 1 Bark at low freq)
    // simple local spreading, O(n*16)
    for i in 0..n {
        let level = mag_db[i];
        if level < -80.0 {
            continue;
        }
        for j in 0..n {
            if i == j {
                continue;
            }
            let dbark = (bark[j] - bark[i]).abs();
            if dbark > 2.0 {
                continue;
            }
            // spreading function: triangular, -10 dB/Bark upward, -25 dB/Bark downward
            let spread = if bark[j] > bark[i] {
                15.0 + 10.0 * dbark
            } else {
                15.0 + 25.0 * dbark
            };
            let candidate = level - spread;
            if candidate > thresh[j] {
                thresh[j] = candidate;
            }
        }
    }
    thresh
}

// ── noise profile estimation ─────────────────────────────────────────────────

pub fn estimate_noise_profile(buffer: &AudioBuffer, cfg: &NoiseReductionConfig) -> NoiseProfile {
    let n = cfg.window_size;
    let hop = cfg.hop_size;
    let hann = hann_window(n);
    let n_bins = n / 2 + 1;
    let channels = buffer.channels.count();
    let frames_needed = (buffer.num_frames() / hop).max(1);

    // collect magnitudes per frame
    let mut all_mags: Vec<Vec<f64>> = Vec::new();
    // de-interleave for estimation: use mono mix for linked, or average
    let mono: Vec<f64> = if channels == 1 {
        buffer.samples.clone()
    } else {
        buffer
            .samples
            .chunks_exact(2)
            .map(|p| (p[0] + p[1]) * 0.5)
            .collect()
    };

    let mut pos = 0usize;
    while pos + n <= mono.len() {
        let mut frame = vec![0.0; n];
        for i in 0..n {
            frame[i] = mono[pos + i] * hann[i];
        }
        let spec = real_fft(&frame);
        let mags: Vec<f64> = spec
            .iter()
            .map(|(re, im)| (re * re + im * im).sqrt().max(1e-12))
            .collect();
        all_mags.push(mags);
        pos += hop;
        if all_mags.len() >= frames_needed.min(200) && pos + n > mono.len() {
            break;
        }
    }
    if all_mags.is_empty() {
        return NoiseProfile {
            magnitudes: vec![1e-9; n_bins],
            sample_rate: buffer.sample_rate,
            window_size: n,
        };
    }
    // 15th percentile per bin as noise floor (robust to loud music frames)
    let mut profile = vec![0.0; n_bins];
    for bin in 0..n_bins {
        let mut vals: Vec<f64> = all_mags.iter().map(|f| f[bin]).collect();
        vals.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let idx = ((vals.len() as f64 * 0.15) as usize).min(vals.len() - 1);
        profile[bin] = vals[idx].max(1e-12);
    }
    // ensure at least small floor to avoid over-suppression
    NoiseProfile {
        magnitudes: profile,
        sample_rate: buffer.sample_rate,
        window_size: n,
    }
}

pub fn learn_noise_profile(noise_sample: &AudioBuffer, cfg: &NoiseReductionConfig) -> NoiseProfile {
    estimate_noise_profile(noise_sample, cfg)
}

// ── spectral gating (kept for reference, used inline for stereo-linked) ─────────
#[allow(dead_code)]
fn apply_gate_to_frame(
    spectrum: &mut [(f64, f64)],
    magnitudes: &[f64],
    noise_mag: &[f64],
    masking_db: Option<&[f64]>,
    cfg: &NoiseReductionConfig,
    smoothed_gain: &mut [f64],
) {
    let n_bins = spectrum.len();
    let floor_gain = 10f64.powf(-cfg.reduction_db / 20.0);

    for bin in 0..n_bins {
        let mag = magnitudes[bin].max(1e-12);
        let noise = noise_mag[bin.min(noise_mag.len() - 1)].max(1e-12);
        let mag_db = 20.0 * mag.log10();
        let noise_db = 20.0 * noise.log10();

        // psychoacoustic masking: if bin is masked, don't gate
        let is_masked = if let Some(mask) = masking_db {
            mag_db < mask[bin] + 3.0 // 3 dB hysteresis
        } else {
            false
        };
        if is_masked {
            // smoothly return to 1.0
            smoothed_gain[bin] = smoothed_gain[bin] * 0.85 + 1.0 * 0.15;
            let g = smoothed_gain[bin];
            spectrum[bin].0 *= g;
            spectrum[bin].1 *= g;
            continue;
        }

        let snr_db = mag_db - noise_db;
        // threshold decides gate knee
        let target_gain = if snr_db < cfg.threshold_db {
            floor_gain
        } else if snr_db > cfg.threshold_db + 12.0 {
            1.0
        } else {
            // soft knee 12 dB
            let t = (snr_db - cfg.threshold_db) / 12.0;
            floor_gain + (1.0 - floor_gain) * t
        };

        // log-domain smoothing: attack fast, release slow
        let alpha = if target_gain < smoothed_gain[bin] { 0.6 } else { 0.85 };
        smoothed_gain[bin] = smoothed_gain[bin] * alpha + target_gain * (1.0 - alpha);
        let g = smoothed_gain[bin].clamp(floor_gain, 1.0);
        spectrum[bin].0 *= g;
        spectrum[bin].1 *= g;
    }
}

// ── OLA processing per channel ───────────────────────────────────────────────

fn process_channels_linked(
    channels_data: &[Vec<f64>],
    profile: &NoiseProfile,
    cfg: &NoiseReductionConfig,
    sample_rate: u32,
) -> Vec<Vec<f64>> {
    let n = cfg.window_size;
    let hop = cfg.hop_size;
    let hann = hann_window(n);
    let n_bins = n / 2 + 1;
    let ch_count = channels_data.len();
    let len = channels_data[0].len();
    let out_len = len + n;

    // freq and bark precomputed
    let bin_freqs: Vec<f64> = (0..n_bins)
        .map(|i| i as f64 * sample_rate as f64 / n as f64)
        .collect();
    let barks: Vec<f64> = bin_freqs.iter().map(|&f| bark_scale(f)).collect();

    let mut outputs = vec![vec![0.0; out_len]; ch_count];
    let mut weights = vec![vec![0.0; out_len]; ch_count];

    // stereo-linked smoothed gain shared across channels
    let mut smoothed_gain = vec![1.0; n_bins];

    let mut pos = 0usize;
    while pos < len {
        // prepare frames per channel
        let mut spectra: Vec<Vec<(f64, f64)>> = Vec::with_capacity(ch_count);
        let mut mags_linked = vec![0.0; n_bins];

        for ch in 0..ch_count {
            let mut frame = vec![0.0; n];
            for i in 0..n {
                let idx = pos + i;
                let s = if idx < len { channels_data[ch][idx] } else { 0.0 };
                frame[i] = s * hann[i];
            }
            let spec = real_fft(&frame);
            // accumulate for linked magnitude
            for (b, (re, im)) in spec.iter().enumerate() {
                let m = (re * re + im * im).sqrt().max(1e-12);
                if ch == 0 {
                    mags_linked[b] = m;
                } else {
                    // linked: max across channels (preserves stereo image)
                    if m > mags_linked[b] {
                        mags_linked[b] = m;
                    }
                }
            }
            spectra.push(spec);
        }

        // masking thresholds from linked magnitudes
        let masking = if cfg.use_masking {
            let mag_db: Vec<f64> = mags_linked.iter().map(|&m| 20.0 * m.log10()).collect();
            Some(masking_thresholds(&mag_db, &bin_freqs, &barks))
        } else {
            None
        };

        // compute gain using linked mags
        let floor_gain = 10f64.powf(-cfg.reduction_db / 20.0);
        let mut gains = vec![1.0; n_bins];
        for bin in 0..n_bins {
            let mag = mags_linked[bin].max(1e-12);
            let noise = profile.magnitudes[bin.min(profile.magnitudes.len() - 1)].max(1e-12);
            let mag_db = 20.0 * mag.log10();
            let noise_db = 20.0 * noise.log10();
            let is_masked = if let Some(ref mask) = masking {
                mag_db < mask[bin] + 3.0
            } else {
                false
            };
            let target = if is_masked {
                1.0
            } else {
                let snr = mag_db - noise_db;
                if snr < cfg.threshold_db {
                    floor_gain
                } else if snr > cfg.threshold_db + 12.0 {
                    1.0
                } else {
                    let t = (snr - cfg.threshold_db) / 12.0;
                    floor_gain + (1.0 - floor_gain) * t
                }
            };
            let alpha = if target < smoothed_gain[bin] { 0.6 } else { 0.85 };
            smoothed_gain[bin] = smoothed_gain[bin] * alpha + target * (1.0 - alpha);
            gains[bin] = smoothed_gain[bin].clamp(floor_gain, 1.0);
        }

        // apply gains phase-locked to each channel's spectrum
        for ch in 0..ch_count {
            for b in 0..n_bins {
                spectra[ch][b].0 *= gains[b];
                spectra[ch][b].1 *= gains[b];
            }
            let time = real_ifft(&spectra[ch], n);
            for i in 0..n {
                let idx = pos + i;
                if idx < out_len {
                    outputs[ch][idx] += time[i] * hann[i];
                    weights[ch][idx] += hann[i] * hann[i];
                }
            }
        }

        pos += hop;
    }

    // normalize OLA
    let mut result = Vec::with_capacity(ch_count);
    for ch in 0..ch_count {
        let mut data = vec![0.0; len];
        for i in 0..len {
            let w = weights[ch][i];
            data[i] = if w > 1e-12 {
                (outputs[ch][i] / w).clamp(-1.0, 1.0)
            } else {
                0.0
            };
        }
        result.push(data);
    }
    result
}

// ── public API ───────────────────────────────────────────────────────────────

pub fn apply_noise_reduction(
    buffer: &mut AudioBuffer,
    profile: &NoiseProfile,
    cfg: &NoiseReductionConfig,
) -> Result<()> {
    if buffer.samples.is_empty() {
        return Ok(());
    }
    let ch_count = buffer.channels.count();
    let len = buffer.num_frames();
    let channels: Vec<Vec<f64>> = (0..ch_count)
        .map(|c| buffer.channel_slice(c))
        .collect();

    let processed = if cfg.stereo_linked && ch_count == 2 {
        process_channels_linked(&channels, profile, cfg, buffer.sample_rate)
    } else {
        // fallback per-channel (still uses linked logic per channel independently)
        // reuse linked function but with single channel
        let mut out = Vec::new();
        for ch_data in channels.iter() {
            let single = vec![ch_data.clone()];
            let res = process_channels_linked(&single, profile, cfg, buffer.sample_rate);
            out.push(res.into_iter().next().unwrap());
        }
        out
    };

    // re-interleave
    let mut out_samples = vec![0.0; len * ch_count];
    for (c, ch_data) in processed.iter().enumerate() {
        for (i, &s) in ch_data.iter().enumerate().take(len) {
            out_samples[i * ch_count + c] = s;
        }
    }
    buffer.samples = out_samples;
    Ok(())
}

pub fn reduce_noise_auto(buffer: &mut AudioBuffer, cfg: &NoiseReductionConfig) -> Result<()> {
    let profile = estimate_noise_profile(buffer, cfg);
    apply_noise_reduction(buffer, &profile, cfg)
}

// Convenience for Track integration
pub fn noise_reduction_profile_from_silence(
    silence: &AudioBuffer,
    cfg: &NoiseReductionConfig,
) -> NoiseProfile {
    estimate_noise_profile(silence, cfg)
}
