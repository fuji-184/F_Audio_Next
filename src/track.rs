use crate::audio::{AudioBuffer, BitDepth, Channels};
use crate::compressor::{MultibandCompressorConfig, CompressorBandConfig};
use crate::de_esser::DeEsserConfig;
use crate::dsp::{self, ResampleConfig};
use crate::eq::{EqBand, apply_eq_band};
use crate::error::Result;
use crate::harmonic_exciter::HarmonicExciterConfig;
use crate::io;
use crate::limiter::{LimiterConfig, LimiterStyle};
use crate::noise_reduction::{NoiseReductionConfig, NoiseProfile};
use crate::normalization::{LoudnessStats, NormalizationConfig, measure_loudness as measure_lufs, normalize_loudness as do_normalize_loudness};
use crate::transient_shaper::TransientShaperConfig;

pub struct Track {
    buffer: AudioBuffer,
}

impl Track {
    pub fn new(buffer: AudioBuffer) -> Self {
        Self { buffer }
    }

    // ── EQ ───────────────────────────────────────────────────────────────────

    /// Apply an EQ band. Chainable.
    ///
    /// # Arguments
    /// * `filter`   — filter type name (e.g. `"digital bell 2"`, `"high cut"`)
    /// * `frequency` — center/cutoff frequency in Hz
    /// * `gain_db`  — gain in dB (positive = boost, negative = cut)
    /// * `q`        — Q factor / bandwidth
    pub fn eq(&mut self, filter: &str, frequency: f64, gain_db: f64, q: f64) -> &mut Self {
        let band = EqBand::new(filter, frequency, gain_db, q)
            .expect("invalid EQ parameters");
        apply_eq_band(
            &mut self.buffer.samples,
            &band,
            self.buffer.sample_rate,
            self.buffer.channels.count(),
        );
        self
    }

    // ── Resampling ───────────────────────────────────────────────────────────

    /// Resample to target rate (mastering-grade Kaiser polyphase). Chainable.
    pub fn resample(&mut self, target_rate: u32) -> &mut Self {
        self.buffer = dsp::resample(&self.buffer, target_rate).expect("resample failed");
        self
    }

    /// Resample with custom config (beta, taps, phase). Chainable.
    pub fn resample_with_config(&mut self, target_rate: u32, cfg: &ResampleConfig) -> &mut Self {
        self.buffer = dsp::resample_with_config(&self.buffer, target_rate, cfg).expect("resample failed");
        self
    }

    /// Convert channels (mono ↔ stereo). Chainable.
    pub fn convert_channels(&mut self, channels: &str) -> &mut Self {
        let target = Channels::from_str(channels).expect("invalid channel mode");
        self.buffer = dsp::convert_channels(&self.buffer, target);
        self
    }

    pub fn to_mono(&mut self) -> &mut Self {
        self.buffer = dsp::convert_channels(&self.buffer, Channels::Mono);
        self
    }

    pub fn to_stereo(&mut self) -> &mut Self {
        self.buffer = dsp::convert_channels(&self.buffer, Channels::Stereo);
        self
    }

    // ── Noise Reduction ──────────────────────────────────────────────────────

    /// Apply mastering-grade noise reduction (spectral gating OLA + masking). Chainable.
    pub fn reduce_noise(&mut self, profile: &NoiseProfile, cfg: &NoiseReductionConfig) -> &mut Self {
        crate::noise_reduction::apply_noise_reduction(&mut self.buffer, profile, cfg)
            .expect("noise reduction failed");
        self
    }

    /// Auto-estimate noise profile and reduce. Chainable.
    pub fn reduce_noise_auto(&mut self, cfg: &NoiseReductionConfig) -> &mut Self {
        crate::noise_reduction::reduce_noise_auto(&mut self.buffer, cfg)
            .expect("noise reduction failed");
        self
    }

    /// Learn noise profile from a silence/noise sample.
    pub fn noise_profile(&self, cfg: &NoiseReductionConfig) -> NoiseProfile {
        crate::noise_reduction::estimate_noise_profile(&self.buffer, cfg)
    }

    // ── Compressor ───────────────────────────────────────────────────────────

    /// Multi-band look-ahead feed-forward compressor (log-domain, RMS+Peak, auto-release). Chainable.
    pub fn compress_multiband(&mut self, cfg: &MultibandCompressorConfig) -> &mut Self {
        crate::compressor::compress_multiband(&mut self.buffer, cfg).expect("compressor failed");
        self
    }

    /// Single-band compressor (convenience).
    pub fn compress(&mut self, band_cfg: &CompressorBandConfig, lookahead_ms: f64) -> &mut Self {
        crate::compressor::compress_singleband(&mut self.buffer, band_cfg, lookahead_ms)
            .expect("compressor failed");
        self
    }

    // ── Harmonic Exciter ─────────────────────────────────────────────────────

    /// Oversampled multi-band dynamic wave-shaper (even/odd/tape). Chainable.
    pub fn excite_harmonics(&mut self, cfg: &HarmonicExciterConfig) -> &mut Self {
        crate::harmonic_exciter::apply_harmonic_exciter(&mut self.buffer, cfg)
            .expect("harmonic exciter failed");
        self
    }

    /// Alias for `excite_harmonics`.
    pub fn harmonic_exciter(&mut self, cfg: &HarmonicExciterConfig) -> &mut Self {
        self.excite_harmonics(cfg)
    }

    // ── Transient Shaper ─────────────────────────────────────────────────────

    /// Sub-band envelope-follower transient designer (differential log). Chainable.
    pub fn shape_transients(&mut self, cfg: &TransientShaperConfig) -> &mut Self {
        crate::transient_shaper::apply_transient_shaper(&mut self.buffer, cfg)
            .expect("transient shaper failed");
        self
    }

    /// Alias.
    pub fn transient_shaper(&mut self, cfg: &TransientShaperConfig) -> &mut Self {
        self.shape_transients(cfg)
    }

    // ── De-Esser ─────────────────────────────────────────────────────────────

    /// Phase-locked multi-band dynamic variable-Q de-esser. Chainable.
    pub fn de_ess(&mut self, cfg: &DeEsserConfig) -> &mut Self {
        crate::de_esser::apply_de_esser(&mut self.buffer, cfg).expect("de-esser failed");
        self
    }

    /// Alias.
    pub fn de_esser(&mut self, cfg: &DeEsserConfig) -> &mut Self {
        self.de_ess(cfg)
    }

    // ── Normalization (EBU R128) ─────────────────────────────────────────────

    /// Measure integrated loudness (LUFS) and true-peak (dBTP).
    pub fn loudness(&self) -> LoudnessStats {
        measure_lufs(&self.buffer)
    }

    /// Measure integrated loudness only.
    pub fn integrated_lufs(&self) -> f64 {
        measure_lufs(&self.buffer).integrated_lufs
    }

    /// True-peak in dBTP.
    pub fn true_peak_db(&self) -> f64 {
        measure_lufs(&self.buffer).true_peak_db
    }

    /// Normalize to target LUFS with true-peak ceiling (EBU R128). Chainable.
    /// Example: `track.normalize_loudness(-14.0, -1.0)` for Spotify/Apple.
    pub fn normalize_loudness(&mut self, target_lufs: f64, ceiling_db: f64) -> &mut Self {
        let cfg = NormalizationConfig { target_lufs, true_peak_ceiling_db: ceiling_db };
        do_normalize_loudness(&mut self.buffer, cfg).expect("normalization failed");
        self
    }

    /// Normalize to -14 LUFS / -1 dBTP (streaming standard). Chainable.
    pub fn normalize_streaming(&mut self) -> &mut Self {
        self.normalize_loudness(-14.0, -1.0)
    }

    // ── Limiter ──────────────────────────────────────────────────────────────

    /// True-peak brickwall limiter with look-ahead and oversampling. Chainable.
    pub fn limit(&mut self, cfg: &LimiterConfig) -> &mut Self {
        crate::limiter::apply_limiter(&mut self.buffer, cfg).expect("limiter failed");
        self
    }

    /// Convenience: limit to ceiling (default Transparent, 2ms lookahead, 4×).
    pub fn limit_to(&mut self, ceiling_db: f64) -> &mut Self {
        let cfg = LimiterConfig { ceiling_db, ..Default::default() };
        self.limit(&cfg)
    }

    /// Limit with style selection.
    pub fn limit_with_style(&mut self, ceiling_db: f64, style: LimiterStyle) -> &mut Self {
        let cfg = LimiterConfig { ceiling_db, style, ..Default::default() };
        self.limit(&cfg)
    }

    // ── Save ─────────────────────────────────────────────────────────────────

    /// Save as WAV.
    ///
    /// * `sample_rate` — target sample rate in Hz (e.g. 44100, 48000)
    /// * `channels`    — `"mono"` or `"stereo"`
    /// * `bit_depth`   — `16`, `24`, or `32`
    pub fn save_wav(&self, path: &str, sample_rate: u32, channels: &str, bit_depth: u16) -> Result<()> {
        let target_ch  = Channels::from_str(channels)?;
        let depth      = BitDepth::from_u16(bit_depth)?;
        let out        = dsp::convert_sample_rate_and_channels(&self.buffer, sample_rate, target_ch)?;
        io::write_wav(io::ensure_ext(path, "wav"), &out, depth)
    }

    /// Save as FLAC.
    ///
    /// * `sample_rate` — target sample rate in Hz
    /// * `channels`    — `"mono"` or `"stereo"`
    /// * `bit_depth`   — `16`, `24`, or `32`
    pub fn save_flac(&self, path: &str, sample_rate: u32, channels: &str, bit_depth: u16) -> Result<()> {
        let target_ch = Channels::from_str(channels)?;
        let depth     = BitDepth::from_u16(bit_depth)?;
        let out       = dsp::convert_sample_rate_and_channels(&self.buffer, sample_rate, target_ch)?;
        io::write_flac(io::ensure_ext(path, "flac"), &out, depth)
    }

    /// Save as MP3.
    ///
    /// * `bitrate_kbps` — bitrate in kbps (e.g. 128, 192, 320)
    /// * `channels`     — `"mono"` or `"stereo"`
    pub fn save_mp3(&self, path: &str, bitrate_kbps: u32, channels: &str) -> Result<()> {
        let target_ch = Channels::from_str(channels)?;
        let out       = dsp::convert_sample_rate_and_channels(&self.buffer, self.buffer.sample_rate, target_ch)?;
        io::write_mp3(io::ensure_ext(path, "mp3"), &out, bitrate_kbps)
    }

    pub fn sample_rate(&self) -> u32       { self.buffer.sample_rate }
    pub fn channels(&self) -> Channels     { self.buffer.channels }
    pub fn num_frames(&self) -> usize      { self.buffer.num_frames() }
    pub fn duration_seconds(&self) -> f64  { self.num_frames() as f64 / self.sample_rate() as f64 }
    pub fn buffer(&self) -> &AudioBuffer   { &self.buffer }
    pub fn buffer_mut(&mut self) -> &mut AudioBuffer { &mut self.buffer }
}
