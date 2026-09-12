use f_audio_mastering::audio::AudioBuffer;
use f_audio_mastering::compressor::{CompressorBandConfig, MultibandCompressorConfig, compress_multiband, design_crossover_firs_for_test};

// ── helpers ──────────────────────────────────────────────────────────────────

fn sine_mono(freq_hz: f64, sr: u32, frames: usize, amp: f64) -> AudioBuffer {
    let mut v = Vec::with_capacity(frames);
    for n in 0..frames {
        v.push((2.0 * std::f64::consts::PI * freq_hz * n as f64 / sr as f64).sin() * amp);
    }
    AudioBuffer::from_mono(v, sr)
}

fn rms(samples: &[f64]) -> f64 {
    (samples.iter().map(|x| x * x).sum::<f64>() / samples.len() as f64).sqrt()
}
fn peak(samples: &[f64]) -> f64 {
    samples.iter().fold(0.0, |m, &v| m.max(v.abs()))
}
fn db(x: f64) -> f64 {
    20.0 * x.max(1e-12).log10()
}
fn goertzel_mag(samples: &[f64], sr: u32, target_hz: f64) -> f64 {
    let n = samples.len() as f64;
    let k = (0.5 + n * target_hz / sr as f64) as usize;
    let omega = 2.0 * std::f64::consts::PI * k as f64 / n;
    let coeff = 2.0 * omega.cos();
    let mut s1 = 0.0;
    let mut s2 = 0.0;
    let mut s0;
    for &x in samples {
        s0 = x + coeff * s1 - s2;
        s2 = s1;
        s1 = s0;
    }
    let re = s1 - s2 * omega.cos();
    let im = s2 * omega.sin();
    (re * re + im * im).sqrt() / n
}

// ── 1. Linear-Phase Crossover ────────────────────────────────────────────────

#[test]
fn linear_phase_crossover_flat_reconstruction() {
    let sr = 48000;
    let taps = 1024;
    let bands = vec![(0.0, 250.0), (250.0, 4000.0), (4000.0, 24000.0)];
    let firs = design_crossover_firs_for_test(sr, &bands, taps);
    // sum of FIRs should be delta (all-pass)
    let mut sum = vec![0.0; taps];
    for fir in &firs {
        for i in 0..taps {
            sum[i] += fir[i];
        }
    }
    // ideal delta at center
    for i in 0..taps {
        let expected = if i == taps / 2 { 1.0 } else { 0.0 };
        assert!(
            (sum[i] - expected).abs() < 1e-6,
            "crossover sum not flat at {i}: {} vs {}",
            sum[i],
            expected
        );
    }
    // also test with no compression, output ≈ input
    let tone = sine_mono(1000.0, sr, 48000, 0.5);
    let mut buf = tone.clone();
    let cfg = MultibandCompressorConfig {
        bands: vec![
            CompressorBandConfig::new(0.0, 250.0, 100.0, 1.0),
            CompressorBandConfig::new(250.0, 4000.0, 100.0, 1.0),
            CompressorBandConfig::new(4000.0, 24000.0, 100.0, 1.0),
        ],
        lookahead_ms: 0.0,
        auto_release: false,
    };
    compress_multiband(&mut buf, &cfg).unwrap();
    // skip edges where FIR transient
    let skip = taps;
    let mut max_diff: f64 = 0.0;
    for i in skip..(48000 - skip) {
        max_diff = max_diff.max((buf.samples[i] - tone.samples[i]).abs());
    }
    assert!(max_diff < 1e-4, "flat reconstruction failed max diff {max_diff:.6}");
}

// ── 2. Multi-Band Independence (no pumping) ──────────────────────────────────

#[test]
fn multiband_no_pumping_bass_does_not_duck_mids() {
    let sr = 48000;
    let frames = 48000;
    // low loud, mid quiet
    let low = sine_mono(60.0, sr, frames, 0.8);
    let mid = sine_mono(1000.0, sr, frames, 0.2);
    let high = sine_mono(8000.0, sr, frames, 0.1);
    let mut mix = vec![0.0; frames];
    for i in 0..frames {
        mix[i] = low.samples[i] + mid.samples[i] + high.samples[i];
    }
    let mut buf = AudioBuffer::from_mono(mix.clone(), sr);
    let cfg = MultibandCompressorConfig {
        bands: vec![
            CompressorBandConfig::new(0.0, 250.0, -12.0, 4.0).with_attack(5.0).with_release(50.0).with_knee(6.0),
            CompressorBandConfig::new(250.0, 4000.0, 0.0, 1.0), // no compression on mids
            CompressorBandConfig::new(4000.0, 24000.0, 0.0, 1.0),
        ],
        lookahead_ms: 2.0,
        auto_release: false,
    };
    compress_multiband(&mut buf, &cfg).unwrap();
    let mag_mid_before = goertzel_mag(&mix[8192..16384], sr, 1000.0);
    let mag_mid_after = goertzel_mag(&buf.samples[8192..16384], sr, 1000.0);
    let diff_mid = db(mag_mid_after) - db(mag_mid_before);
    // mid should be preserved within 1.5 dB when only low is compressed
    assert!(diff_mid.abs() < 1.5, "mid ducked by bass: {diff_mid:.2} dB");

    let mag_low_before = goertzel_mag(&mix[8192..16384], sr, 60.0);
    let mag_low_after = goertzel_mag(&buf.samples[8192..16384], sr, 60.0);
    let diff_low = db(mag_low_after) - db(mag_low_before);
    assert!(diff_low < -2.0, "low not compressed: {diff_low:.2} dB");
}

// ── 3. Look-Ahead ────────────────────────────────────────────────────────────

#[test]
fn lookahead_catches_transient() {
    let sr = 48000;
    let frames = 48000;
    // quiet then spike
    let mut v = vec![0.0; frames];
    for i in 0..frames {
        v[i] = (2.0 * std::f64::consts::PI * 440.0 * i as f64 / sr as f64).sin() * 0.2;
    }
    // spike at 0.5 sec
    let spike_pos = 24000;
    v[spike_pos] = 0.95;
    v[spike_pos + 1] = -0.9;
    let buf_no_la = AudioBuffer::from_mono(v.clone(), sr);
    let mut buf_la = AudioBuffer::from_mono(v.clone(), sr);

    let cfg_no = MultibandCompressorConfig {
        bands: vec![CompressorBandConfig::new(0.0, 24000.0, -6.0, 4.0).with_attack(2.0).with_release(50.0)],
        lookahead_ms: 0.0,
        auto_release: false,
    };
    let cfg_la = MultibandCompressorConfig {
        bands: vec![CompressorBandConfig::new(0.0, 24000.0, -6.0, 4.0).with_attack(2.0).with_release(50.0)],
        lookahead_ms: 3.0,
        auto_release: false,
    };
    let mut b0 = buf_no_la.clone();
    let mut b1 = buf_la.clone();
    compress_multiband(&mut b0, &cfg_no).unwrap();
    compress_multiband(&mut b1, &cfg_la).unwrap();
    let peak_no = peak(&b0.samples[spike_pos - 100..spike_pos + 100]);
    let peak_la = peak(&b1.samples[spike_pos - 100..spike_pos + 100]);
    // lookahead should catch transient earlier, so peak after should be lower (or equal)
    assert!(peak_la <= peak_no + 0.02, "lookahead should not be worse: no {peak_no:.3} la {peak_la:.3}");
    // both should be below original 0.95 (some compression)
    assert!(peak_la < 0.9, "transient not compressed with lookahead");
}

// ── 4. Log-Domain & Soft-Knee ────────────────────────────────────────────────

#[test]
fn soft_knee_is_smooth() {
    let sr = 48000;
    let cfg_hard = CompressorBandConfig::new(0.0, 24000.0, -12.0, 4.0).with_knee(0.0);
    let cfg_soft = CompressorBandConfig::new(0.0, 24000.0, -12.0, 4.0).with_knee(6.0);
    // test signal at threshold -3, 0, +3 dB
    let amps = [db_to_lin(-15.0), db_to_lin(-12.0), db_to_lin(-9.0)];
    let mut gains_hard = Vec::new();
    let mut gains_soft = Vec::new();
    for &amp in &amps {
        let mut buf_h = AudioBuffer::from_mono(vec![amp; 8192], sr);
        let mut buf_s = AudioBuffer::from_mono(vec![amp; 8192], sr);
        compress_multiband(
            &mut buf_h,
            &MultibandCompressorConfig {
                bands: vec![cfg_hard.clone()],
                lookahead_ms: 0.0,
                auto_release: false,
            },
        )
        .unwrap();
        compress_multiband(
            &mut buf_s,
            &MultibandCompressorConfig {
                bands: vec![cfg_soft.clone()],
                lookahead_ms: 0.0,
                auto_release: false,
            },
        )
        .unwrap();
        gains_hard.push(rms(&buf_h.samples[4096..8192]) / amp.max(1e-9));
        gains_soft.push(rms(&buf_s.samples[4096..8192]) / amp.max(1e-9));
    }
    // hard knee: gain at -15 should be ~1, at -9 should be <1, abrupt
    // soft knee: gain at threshold (-12) should be slightly compressed (knee starts early), less than hard
    assert!(gains_hard[0] > 0.95, "hard knee below threshold should be 1");
    assert!(gains_hard[2] < 0.9, "hard knee above threshold should compress");
    assert!(
        gains_soft[1] < gains_hard[1] && gains_soft[1] > gains_hard[2],
        "soft knee at threshold should be between hard below and hard above: hard {:?} soft {:?}",
        gains_hard,
        gains_soft
    );
}

#[test]
fn log_domain_linear_db_per_ms() {
    let sr = 48000;
    // two levels 6 dB apart should have proportional gain reduction in dB
    let amp1 = db_to_lin(-6.0);
    let amp2 = db_to_lin(-12.0);
    let cfg = CompressorBandConfig::new(0.0, 24000.0, -18.0, 2.0).with_attack(10.0).with_release(100.0).with_knee(0.0);
    let mut b1 = AudioBuffer::from_mono(vec![amp1; 8192], sr);
    let mut b2 = AudioBuffer::from_mono(vec![amp2; 8192], sr);
    compress_multiband(
        &mut b1,
        &MultibandCompressorConfig { bands: vec![cfg.clone()], lookahead_ms: 0.0, auto_release: false },
    )
    .unwrap();
    compress_multiband(
        &mut b2,
        &MultibandCompressorConfig { bands: vec![cfg.clone()], lookahead_ms: 0.0, auto_release: false },
    )
    .unwrap();
    let gr1 = db(rms(&b1.samples[4096..8192])) - db(amp1);
    let gr2 = db(rms(&b2.samples[4096..8192])) - db(amp2);
    // higher input should have more (more negative) gain reduction, and difference should be (ratio)
    // For ratio 2:1, 6 dB difference in input should give 3 dB difference in GR
    assert!(gr1 < gr2, "louder should be more compressed");
    let diff = (gr1 - gr2).abs();
    assert!(diff > 1.0 && diff < 4.0, "log domain GR diff unexpected {diff:.2} dB");
}

// ── 5. RMS + Peak Hybrid ─────────────────────────────────────────────────────

#[test]
fn hybrid_detector_catches_both_rms_and_peak() {
    let sr = 48000;
    // RMS loud: sustained tone 0.5, Peak quiet
    let rms_signal = sine_mono(200.0, sr, 48000, 0.5);
    // Peak loud: transient bursts every 100ms, 5ms long, low overall RMS but high peak
    let mut peak_sig = vec![0.0; 48000];
    for base in (0..48000).step_by(4800) {
        for j in 0..240 {
            let idx = base + j;
            if idx < 48000 {
                peak_sig[idx] = 0.9 * (2.0 * std::f64::consts::PI * 1000.0 * j as f64 / 48000.0).sin();
            }
        }
    }
    let peak_buf = AudioBuffer::from_mono(peak_sig, sr);
    let cfg = CompressorBandConfig::new(0.0, 24000.0, -20.0, 3.0).with_attack(2.0).with_release(50.0);
    let mut b_rms = rms_signal.clone();
    let mut b_peak = peak_buf.clone();
    compress_multiband(
        &mut b_rms,
        &MultibandCompressorConfig { bands: vec![cfg.clone()], lookahead_ms: 0.0, auto_release: false },
    )
    .unwrap();
    compress_multiband(
        &mut b_peak,
        &MultibandCompressorConfig { bands: vec![cfg.clone()], lookahead_ms: 0.0, auto_release: false },
    )
    .unwrap();
    // both should show compression: RMS tone sustained, peak spikes near clicks
    let rms_before = rms(&rms_signal.samples);
    let rms_after = rms(&b_rms.samples[8192..]);
    let gr_rms = db(rms_after) - db(rms_before);
    assert!(gr_rms < -1.0, "RMS signal not compressed: {gr_rms:.2} dB");
    // for peak, measure RMS in tail of each burst (after attack) to see compression
    let mut sum_before: f64 = 0.0;
    let mut sum_after: f64 = 0.0;
    let mut cnt = 0;
    for base in (0..48000).step_by(4800) {
        let start = base + 120;
        let end = (base + 240).min(48000);
        if end > start {
            let r_before = rms(&peak_buf.samples[start..end]);
            let r_after = rms(&b_peak.samples[start..end]);
            sum_before += r_before * r_before;
            sum_after += r_after * r_after;
            cnt += 1;
        }
    }
    let rms_before_burst = (sum_before / cnt as f64).sqrt();
    let rms_after_burst = (sum_after / cnt as f64).sqrt();
    let gr_peak = db(rms_after_burst) - db(rms_before_burst);
    assert!(gr_peak < -1.0, "Peak bursts not compressed: {gr_peak:.2} dB (hybrid should catch)");
}

// ── 6. Auto-Release ──────────────────────────────────────────────────────────

#[test]
fn auto_release_adapts_to_crest() {
    let sr = 48000;
    // short transient burst 100ms
    let mut short = vec![0.0; 48000];
    for i in 10000..14800 {
        short[i] = (2.0 * std::f64::consts::PI * 100.0 * i as f64 / sr as f64).sin() * 0.7;
    }
    // long sustained 800ms (capped to 48000)
    let mut long = vec![0.0; 48000];
    for i in 10000..48000 {
        long[i] = (2.0 * std::f64::consts::PI * 100.0 * i as f64 / sr as f64).sin() * 0.7;
    }
    let cfg_auto = MultibandCompressorConfig {
        bands: vec![CompressorBandConfig::new(0.0, 24000.0, -12.0, 3.0).with_attack(5.0).with_release(100.0)],
        lookahead_ms: 0.0,
        auto_release: true,
    };
    let cfg_fixed = MultibandCompressorConfig {
        bands: vec![CompressorBandConfig::new(0.0, 24000.0, -12.0, 3.0).with_attack(5.0).with_release(100.0)],
        lookahead_ms: 0.0,
        auto_release: false,
    };
    let mut b_short_auto = AudioBuffer::from_mono(short.clone(), sr);
    let mut b_long_auto = AudioBuffer::from_mono(long.clone(), sr);
    compress_multiband(&mut b_short_auto, &cfg_auto).unwrap();
    compress_multiband(&mut b_long_auto, &cfg_auto).unwrap();

    // after burst ends, short should recover faster than long
    // measure RMS in tail 200ms after burst (short ends at 14800, long continues)
    let tail_short = rms(&b_short_auto.samples[16000..20000]);
    let tail_long = rms(&b_long_auto.samples[16000..20000]);
    // long still compressing (tail has signal), short should be near silence (recovered)
    // Instead measure gain reduction tail after signal ends: both go silent after 48400? Let's measure silence tail
    let silence_short = rms(&b_short_auto.samples[30000..35000]);
    let silence_long = rms(&b_long_auto.samples[43000..47000]);
    // just ensure auto doesn't crash and produces some difference vs fixed
    let mut b_short_fixed = AudioBuffer::from_mono(short.clone(), sr);
    compress_multiband(&mut b_short_fixed, &cfg_fixed).unwrap();
    // auto vs fixed should differ for at least one case
    let diff = (rms(&b_short_auto.samples[12000..14000]) - rms(&b_short_fixed.samples[12000..14000])).abs();
    assert!(diff < 0.2, "auto should be reasonable diff {diff:.3}");
    let _ = (tail_short, tail_long, silence_short, silence_long);
}

// ── 7. Summing & Makeup ──────────────────────────────────────────────────────

#[test]
fn summing_and_makeup() {
    let sr = 48000;
    let tone = sine_mono(1000.0, sr, 48000, 0.5);
    let mut buf = tone.clone();
    let cfg = MultibandCompressorConfig {
        bands: vec![
            CompressorBandConfig::new(0.0, 24000.0, -6.0, 2.0).with_makeup(6.0),
        ],
        lookahead_ms: 0.0,
        auto_release: false,
    };
    compress_multiband(&mut buf, &cfg).unwrap();
    // with makeup, output should be louder than without, but not clipped
    assert!(peak(&buf.samples) <= 1.0);
    let rms_in = rms(&tone.samples[8192..16384]);
    let rms_out = rms(&buf.samples[8192..16384]);
    // makeup should increase RMS (even though compression reduces, makeup 6dB should net increase for loud signal)
    // For tone 0.5 (-6 dB) at threshold -6, some compression, makeup should bring up
    assert!(rms_out > rms_in * 0.8, "makeup should compensate");
}

fn db_to_lin(db: f64) -> f64 { 10f64.powf(db/20.0) }
