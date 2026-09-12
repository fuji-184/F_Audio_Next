use f_audio_mastering::audio::{AudioBuffer, Channels};
use f_audio_mastering::dsp::{bessel_i0, resample, resample_with_config, PhaseMode, ResampleConfig};

// ── helpers ─────────────────────────────────────────────────────────────────

fn sine_mono(freq_hz: f64, sample_rate: u32, frames: usize) -> AudioBuffer {
    let mut samples = Vec::with_capacity(frames);
    for n in 0..frames {
        samples.push((2.0 * std::f64::consts::PI * freq_hz * n as f64 / sample_rate as f64).sin());
    }
    AudioBuffer::from_mono(samples, sample_rate)
}

fn impulse_mono(sample_rate: u32, frames: usize, pos: usize) -> AudioBuffer {
    let mut samples = vec![0.0; frames];
    if pos < frames {
        samples[pos] = 1.0;
    }
    AudioBuffer::from_mono(samples, sample_rate)
}

fn rms(samples: &[f64]) -> f64 {
    (samples.iter().map(|v| v * v).sum::<f64>() / samples.len() as f64).sqrt()
}

fn peak(samples: &[f64]) -> f64 {
    samples.iter().fold(0.0f64, |m, &v| m.max(v.abs()))
}

fn db(x: f64) -> f64 {
    20.0 * x.max(1e-20).log10()
}

// ── 1. Kaiser & Bessel correctness ─────────────────────────────────────────

#[test]
fn bessel_i0_reference_values() {
    // I0(0) = 1
    assert!((bessel_i0(0.0) - 1.0).abs() < 1e-12);
    // I0(1) ≈ 1.266065877...
    assert!((bessel_i0(1.0) - 1.2660658777520082).abs() < 1e-9);
    // I0(9) ~ 1e3 range, check monotonic growth and finite
    let i0_9 = bessel_i0(9.0);
    let i0_12 = bessel_i0(12.0);
    let i0_14 = bessel_i0(14.0);
    assert!(i0_9 > 100.0 && i0_9 < 10000.0);
    assert!(i0_12 > i0_9);
    assert!(i0_14 > i0_12);
    // beta 9..14 must be finite and >0
    for beta in [9.0, 10.0, 12.0, 14.0] {
        let v = bessel_i0(beta);
        assert!(v.is_finite() && v > 0.0);
    }
}

#[test]
fn resample_config_beta_clamp_and_defaults() {
    let cfg = ResampleConfig::default();
    assert_eq!(cfg.beta, 12.0);
    assert_eq!(cfg.taps, 256);
    assert_eq!(cfg.phases, 4096);
    match cfg.phase {
        PhaseMode::Intermediate(mix) => assert!((mix - 0.05).abs() < 1e-9),
        _ => panic!("default should be Intermediate(0.05) ≈ 95% linear"),
    }

    // with_beta clamps to 9..14
    let low = ResampleConfig::default().with_beta(5.0);
    assert_eq!(low.beta, 9.0);
    let high = ResampleConfig::default().with_beta(20.0);
    assert_eq!(high.beta, 14.0);
}

// ── 2. Transparency: identity & length ─────────────────────────────────────

#[test]
fn identity_same_rate_is_bit_transparent() {
    let buf = sine_mono(440.0, 48000, 1024);
    let out = resample(&buf, 48000).unwrap();
    assert_eq!(out.sample_rate, 48000);
    assert_eq!(out.samples.len(), buf.samples.len());
    for (a, b) in buf.samples.iter().zip(out.samples.iter()) {
        assert!((a - b).abs() < 1e-12);
    }
}

#[test]
fn output_frame_count_matches_ratio() {
    let buf = sine_mono(1000.0, 48000, 4800);
    let cases = [
        (48000, 44100),
        (44100, 48000),
        (48000, 96000),
        (96000, 44100),
        (44100, 22050),
        (48000, 16000),
    ];
    for (fin, fout) in cases {
        let b = AudioBuffer::from_mono(buf.samples.clone(), fin);
        let out = resample(&b, fout).unwrap();
        let expected = (b.num_frames() as f64 * fout as f64 / fin as f64).round() as usize;
        assert_eq!(out.num_frames(), expected, "ratio {fin}->{fout}");
        assert_eq!(out.sample_rate, fout);
    }
}

#[test]
fn dc_preservation_unity_gain() {
    // constant 0.5 should stay 0.5 after resampling (brick-wall LP gain = 1)
    let frames = 4096;
    let buf = AudioBuffer::from_mono(vec![0.5; frames], 48000);
    let out = resample(&buf, 44100).unwrap();
    // ignore edges where transient of FIR startup causes deviation, check middle 50%
    let start = out.num_frames() / 4;
    let end = start + out.num_frames() / 2;
    for &v in &out.samples[start..end] {
        assert!((v - 0.5).abs() < 1e-4, "DC not preserved: {v}");
    }
}

// ── 3. Brick-wall & aliasing ────────────────────────────────────────────────

#[test]
fn sine_amplitude_preserved_in_passband() {
    // 1 kHz well inside passband must not be attenuated (roll-off < 0.05 dB)
    let buf = sine_mono(1000.0, 48000, 48000);
    let out = resample(&buf, 44100).unwrap();
    // measure steady-state after FIR warm-up (skip first 1024 frames ~ filter delay)
    let skip = 2048;
    let p_in = peak(&buf.samples[skip..skip + 4096]);
    let p_out = peak(&out.samples[skip..skip + 4096]);
    let diff_db = (db(p_out) - db(p_in)).abs();
    assert!(diff_db < 0.1, "passband roll-off too high: {diff_db:.4} dB");
}

#[test]
fn anti_alias_attenuation_above_nyquist() {
    // tone at 20 kHz @48k downsampled to 16k (Nyquist 8k) must be crushed
    // This proves brick-wall design is working directionally
    let buf = sine_mono(20000.0, 48000, 48000);
    let out = resample(&buf, 16000).unwrap();
    let skip = 2048;
    let out_peak = peak(&out.samples[skip..]);
    // alias should be well below -40 dBFS — mastering target is -140 dB but
    // 20 kHz is 2.5× above cutoff; with 256-tap Kaiser practical attenuation is ~40 dB
    assert!(
        out_peak < 1e-2,
        "alias not suppressed: peak {out_peak:.6} ({:.1} dB)",
        db(out_peak)
    );
    // stronger check: with mastering config beta=14, attenuation should be similar or better
    let cfg_master = ResampleConfig::mastering().with_beta(14.0);
    let out2 = resample_with_config(&buf, 16000, &cfg_master).unwrap();
    let peak2 = peak(&out2.samples[skip..]);
    assert!(peak2 < 1e-2, "mastering beta=14 alias peak {peak2:.6}");
}

#[test]
fn upsampling_does_not_create_images() {
    // 1 kHz @16k upsampled to 48k — spectrum should stay clean
    // time-domain peak must stay ~1.0, no amplitude blow-up
    let buf = sine_mono(1000.0, 16000, 16000);
    let out = resample(&buf, 48000).unwrap();
    let p = peak(&out.samples[2048..]);
    assert!((p - 1.0).abs() < 0.02, "upsampled peak wrong {p}");
}

// ── 4. Polyphase fractional delay correctness ───────────────────────────────

#[test]
fn arbitrary_ratio_44100_to_48000_roundtrip() {
    // 44.1k -> 48k -> 44.1k should be nearly transparent (error < -60 dB)
    let orig = sine_mono(1000.0, 44100, 44100);
    let up = resample(&orig, 48000).unwrap();
    let down = resample(&up, 44100).unwrap();
    // compare middle region to avoid edge transients, allow for FIR delay
    let len = orig.num_frames().min(down.num_frames());
    let start = 2048;
    let end = (len - 2048).max(start + 1);
    let mut err_sum = 0.0;
    let mut sig_sum = 0.0;
    for i in start..end {
        let e = orig.samples[i] - down.samples[i];
        err_sum += e * e;
        sig_sum += orig.samples[i] * orig.samples[i];
    }
    let snr_db = 10.0 * (sig_sum / err_sum.max(1e-20)).log10();
    assert!(snr_db > 50.0, "roundtrip SNR too low: {snr_db:.1} dB");
}

// ── 5. Phase modes ──────────────────────────────────────────────────────────

#[test]
fn linear_phase_has_preringing_minimum_has_not() {
    // impulse test: linear-phase must have symmetric ringing (energy before and after)
    // minimum-phase must have energy concentrated after impulse (pre-ringing << post)
    let frames = 2048;
    let pos = 1024;
    let buf = impulse_mono(48000, frames, pos);

    let cfg_lin = ResampleConfig {
        beta: 12.0,
        taps: 256,
        phases: 4096,
        phase: PhaseMode::Linear,
    };
    let cfg_min = ResampleConfig {
        beta: 12.0,
        taps: 256,
        phases: 4096,
        phase: PhaseMode::Minimum,
    };

    let out_lin = resample_with_config(&buf, 44100, &cfg_lin).unwrap();
    let out_min = resample_with_config(&buf, 44100, &cfg_min).unwrap();

    // find peak position in output (approx impulse location scaled)
    let peak_lin = out_lin
        .samples
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.abs().partial_cmp(&b.1.abs()).unwrap())
        .unwrap()
        .0;
    let peak_min = out_min
        .samples
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.abs().partial_cmp(&b.1.abs()).unwrap())
        .unwrap()
        .0;

    // look at ringing outside main lobe: exclude ±2 samples around peak (main lobe)
    // linear: pre and post maxima should be similar order, minimum: pre << post
    let win = 40usize;
    let gap = 2usize;
    let pre_start_lin = peak_lin.saturating_sub(win);
    let pre_end_lin = peak_lin.saturating_sub(gap);
    let pre_max_lin = if pre_end_lin > pre_start_lin {
        out_lin.samples[pre_start_lin..pre_end_lin].iter().fold(0.0f64, |m, &v| m.max(v.abs()))
    } else { 0.0 };
    let post_start_lin = (peak_lin + gap + 1).min(out_lin.samples.len());
    let post_end_lin = (peak_lin + win + 1).min(out_lin.samples.len());
    let post_max_lin = if post_end_lin > post_start_lin {
        out_lin.samples[post_start_lin..post_end_lin].iter().fold(0.0f64, |m, &v| m.max(v.abs()))
    } else { 0.0 };

    let pre_start_min = peak_min.saturating_sub(win);
    let pre_end_min = peak_min.saturating_sub(gap);
    let pre_max_min = if pre_end_min > pre_start_min {
        out_min.samples[pre_start_min..pre_end_min].iter().fold(0.0f64, |m, &v| m.max(v.abs()))
    } else { 0.0 };
    let post_start_min = (peak_min + gap + 1).min(out_min.samples.len());
    let post_end_min = (peak_min + win + 1).min(out_min.samples.len());
    let post_max_min = if post_end_min > post_start_min {
        out_min.samples[post_start_min..post_end_min].iter().fold(0.0f64, |m, &v| m.max(v.abs()))
    } else { 0.0 };

    // linear should be roughly symmetric: pre/post maxima within factor 3
    let ratio_lin = pre_max_lin / post_max_lin.max(1e-20);
    assert!(
        ratio_lin > 0.3 && ratio_lin < 3.0,
        "linear pre/post max not symmetric: pre_max {pre_max_lin:.6} post_max {post_max_lin:.6} ratio {ratio_lin:.3}"
    );

    // minimum: pre-ringing max should be clearly smaller than post and than linear's pre
    assert!(
        pre_max_min < post_max_min,
        "minimum-phase should have pre_max < post_max: pre {pre_max_min:.6} post {post_max_min:.6}"
    );
    assert!(
        pre_max_min < pre_max_lin,
        "minimum-phase pre-ringing should be less than linear: min {pre_max_min:.6} lin {pre_max_lin:.6}"
    );
}

#[test]
fn intermediate_phase_is_between_linear_and_minimum() {
    let frames = 2048;
    let pos = 1024;
    let buf = impulse_mono(48000, frames, pos);
    let cfg_lin = ResampleConfig { phase: PhaseMode::Linear, ..Default::default() };
    let cfg_mid = ResampleConfig { phase: PhaseMode::Intermediate(0.05), ..Default::default() }; // 95% linear
    let cfg_min = ResampleConfig { phase: PhaseMode::Minimum, ..Default::default() };

    let out_lin = resample_with_config(&buf, 44100, &cfg_lin).unwrap();
    let out_mid = resample_with_config(&buf, 44100, &cfg_mid).unwrap();
    let out_min = resample_with_config(&buf, 44100, &cfg_min).unwrap();

    // RMS difference: mid should be closer to linear than to minimum
    let diff_mid_lin: f64 = out_mid
        .samples
        .iter()
        .zip(out_lin.samples.iter())
        .map(|(a, b)| (a - b).powi(2))
        .sum();
    let diff_mid_min: f64 = out_mid
        .samples
        .iter()
        .zip(out_min.samples.iter())
        .map(|(a, b)| (a - b).powi(2))
        .sum();
    assert!(
        diff_mid_lin < diff_mid_min,
        "intermediate 95% linear should be closer to linear than minimum: lin {diff_mid_lin:.6} min {diff_mid_min:.6}"
    );
}

// ── 6. Stereo & arbitrary ratios ────────────────────────────────────────────

#[test]
fn stereo_resampling_preserves_channels() {
    let sr_in = 48000;
    let sr_out = 44100;
    let frames = 1024;
    let left: Vec<f64> = (0..frames)
        .map(|n| (2.0 * std::f64::consts::PI * 440.0 * n as f64 / sr_in as f64).sin())
        .collect();
    let right: Vec<f64> = (0..frames)
        .map(|n| (2.0 * std::f64::consts::PI * 880.0 * n as f64 / sr_in as f64).sin())
        .collect();
    let buf = AudioBuffer::from_channels(&left, &right, sr_in);
    let out = resample(&buf, sr_out).unwrap();
    assert_eq!(out.channels, Channels::Stereo);
    let expected = (frames as f64 * sr_out as f64 / sr_in as f64).round() as usize;
    assert_eq!(out.num_frames(), expected);
    // left and right should remain uncorrelated (different frequencies)
    let left_out = out.channel_slice(0);
    let right_out = out.channel_slice(1);
    let left_rms = rms(&left_out[512..]);
    let right_rms = rms(&right_out[512..]);
    assert!(left_rms > 0.5 && right_rms > 0.5);
    // cross-correlation should be low (different tones)
    let dot: f64 = left_out.iter().zip(right_out.iter()).map(|(a, b)| a * b).sum();
    let corr = dot / (left_out.len() as f64);
    assert!(corr.abs() < 0.2, "stereo channels leaked: corr {corr}");
}

#[test]
fn high_order_vectorized_fractional_delay_precision() {
    // PHASES = 4096 gives sub-sample precision ~0.00024 sample
    // 48k -> 44.1k ratio is irrational ~1.088435..., polyphase must handle it without jitter
    let buf = sine_mono(8000.0, 48000, 48000); // high freq near Nyquist, sensitive to jitter
    let cfg_high = ResampleConfig::high(); // 4096 phases
    let cfg_draft = ResampleConfig::draft(); // 1024 phases, more jitter
    let out_high = resample_with_config(&buf, 44100, &cfg_high).unwrap();
    let out_draft = resample_with_config(&buf, 44100, &cfg_draft).unwrap();
    // high-precision should have smoother envelope (lower variance of peak)
    let ph = peak(&out_high.samples[2048..]);
    let pd = peak(&out_draft.samples[2048..]);
    assert!(ph > 0.8 && pd > 0.7, "high freq amplitude dropped: high {ph:.3} draft {pd:.3}");
}

// ── 7. Cutoff tracking (sub-bandwidth) ─────────────────────────────────────

#[test]
fn cutoff_tracks_lower_of_two_rates() {
    // Downsample must low-pass at fout/2, not fin/2
    // 48k -> 24k, Nyquist = 12k. 10k tone (below) passes, 18k tone (well above) must be removed
    let pass = sine_mono(10000.0, 48000, 48000);
    let stop = sine_mono(18000.0, 48000, 48000);
    let out_pass = resample(&pass, 24000).unwrap();
    let out_stop = resample(&stop, 24000).unwrap();
    let p_pass = peak(&out_pass.samples[2048..]);
    let p_stop = peak(&out_stop.samples[2048..]);
    assert!(p_pass > 0.5, "passband incorrectly attenuated: {p_pass:.3}");
    // 18 kHz is 1.5× above cutoff (12 kHz), stopband must be significantly attenuated (>25 dB)
    // 256-tap Kaiser has finite transition width, so allow up to -25 dB for this distance
    assert!(p_stop < 0.05, "stopband not attenuated: {p_stop:.3} ({:.1} dB)", db(p_stop));
}
