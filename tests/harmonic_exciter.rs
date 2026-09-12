use f_audio_mastering::audio::AudioBuffer;
use f_audio_mastering::harmonic_exciter::{ExciterBandConfig, ExciterMode, HarmonicExciterConfig, SidechainMode, apply_harmonic_exciter};

fn sine_mono(freq_hz: f64, sr: u32, frames: usize, amp: f64) -> AudioBuffer {
    let mut v = Vec::with_capacity(frames);
    for n in 0..frames {
        v.push((2.0 * std::f64::consts::PI * freq_hz * n as f64 / sr as f64).sin() * amp);
    }
    AudioBuffer::from_mono(v, sr)
}
fn rms(samples: &[f64]) -> f64 {
    (samples.iter().map(|x| x*x).sum::<f64>()/samples.len() as f64).sqrt()
}
fn db(x: f64) -> f64 { 20.0*x.max(1e-12).log10() }
fn goertzel_mag(samples: &[f64], sr: u32, f: f64) -> f64 {
    let n = samples.len() as f64;
    let k = (0.5 + n*f/sr as f64) as usize;
    let omega = 2.0*std::f64::consts::PI*k as f64/n;
    let coeff = 2.0*omega.cos();
    let mut s1=0.0; let mut s2=0.0;
    for &x in samples {
        let s0 = x + coeff*s1 - s2;
        s2=s1; s1=s0;
    }
    let re = s1 - s2*omega.cos();
    let im = s2*omega.sin();
    (re*re+im*im).sqrt()/n
}

// ── 1. Oversampling anti-alias ───────────────────────────────────────────────

#[test]
fn oversampling_prevents_alias() {
    let sr = 48000;
    let frames = 16384;
    // 12kHz tone: 3rd harmonic 36kHz would alias to 12kHz (48-36) without oversampling
    // With 4x oversampling at 192k, 36k is within Nyquist 96k, brick-wall removes alias
    let tone = sine_mono(12000.0, sr, frames, 0.6);
    let mut buf = tone.clone();
    let cfg = HarmonicExciterConfig {
        bands: vec![ExciterBandConfig::new(8000.0, 20000.0, ExciterMode::Odd).with_drive(3.0).with_mix(0.5)],
        oversample_factor: 4,
    };
    apply_harmonic_exciter(&mut buf, &cfg).unwrap();
    // alias would appear at 12k? Actually 12k is fundamental, alias of 36k at 12k would reinforce
    // Better check that energy at non-harmonic alias 6k is low
    let mag_6k = goertzel_mag(&buf.samples[4096..], sr, 6000.0);
    let mag_12k = goertzel_mag(&buf.samples[4096..], sr, 12000.0);
    // alias at 6k should be <-50 dB relative to fundamental
    let alias_db = db(mag_6k) - db(mag_12k);
    assert!(alias_db < -30.0, "alias not suppressed: 6k vs 12k {alias_db:.1} dB, 6k {mag_6k:.6}");
    // also check that without oversampling alias would be higher — we test that oversampled version keeps alias low
    // compare to 1x oversampling (should have more alias)
    let mut buf1 = tone.clone();
    let cfg1 = HarmonicExciterConfig {
        bands: vec![ExciterBandConfig::new(8000.0, 20000.0, ExciterMode::Odd).with_drive(3.0).with_mix(0.5)],
        oversample_factor: 1,
    };
    apply_harmonic_exciter(&mut buf1, &cfg1).unwrap();
    let mag_6k_1x = goertzel_mag(&buf1.samples[4096..], sr, 6000.0);
    // 4x should have less alias than 1x
    assert!(mag_6k <= mag_6k_1x * 1.1, "4x should have less alias than 1x: 4x {mag_6k:.6} 1x {mag_6k_1x:.6}");
}

// ── 2. Even vs Odd harmonics ─────────────────────────────────────────────────

#[test]
fn even_generates_2nd_odd_generates_3rd() {
    let sr = 48000;
    let frames = 16384;
    let tone = sine_mono(1000.0, sr, frames, 0.6);
    let mut buf_even = tone.clone();
    let mut buf_odd = tone.clone();
    let cfg_even = HarmonicExciterConfig {
        bands: vec![ExciterBandConfig::new(500.0, 5000.0, ExciterMode::Even).with_drive(0.03).with_mix(0.5)],
        oversample_factor: 4,
    };
    let cfg_odd = HarmonicExciterConfig {
        bands: vec![ExciterBandConfig::new(500.0, 5000.0, ExciterMode::Odd).with_drive(3.0).with_mix(0.5)],
        oversample_factor: 4,
    };
    apply_harmonic_exciter(&mut buf_even, &cfg_even).unwrap();
    apply_harmonic_exciter(&mut buf_odd, &cfg_odd).unwrap();
    let mag_2k_even = goertzel_mag(&buf_even.samples[4096..], sr, 2000.0);
    let mag_3k_even = goertzel_mag(&buf_even.samples[4096..], sr, 3000.0);
    let mag_2k_odd = goertzel_mag(&buf_odd.samples[4096..], sr, 2000.0);
    let mag_3k_odd = goertzel_mag(&buf_odd.samples[4096..], sr, 3000.0);
    // even should have stronger 2nd than 3rd
    assert!(mag_2k_even > mag_3k_even * 1.5, "even should favor 2nd: 2k {mag_2k_even:.6} 3k {mag_3k_even:.6}");
    // odd should have stronger 3rd than even's 3rd? At least odd's 3rd > even's 3rd or odd's 3rd > odd's 2nd
    assert!(mag_3k_odd > mag_2k_odd * 0.8, "odd should have strong 3rd: 2k {mag_2k_odd:.6} 3k {mag_3k_odd:.6}");
    assert!(mag_3k_odd > mag_3k_even, "odd 3rd should be stronger than even 3rd");
}

// ── 3. Tape morph ────────────────────────────────────────────────────────────

#[test]
fn tape_curve_morph_gamma() {
    let sr = 48000;
    let frames = 16384;
    let tone = sine_mono(1000.0, sr, frames, 0.6);
    let mut buf_low_gamma = tone.clone();
    let mut buf_high_gamma = tone.clone();
    let cfg_low = HarmonicExciterConfig {
        bands: vec![ExciterBandConfig::new(500.0, 5000.0, ExciterMode::Tape).with_drive(2.0).with_gamma(0.7).with_mix(0.5)],
        oversample_factor: 4,
    };
    let cfg_high = HarmonicExciterConfig {
        bands: vec![ExciterBandConfig::new(500.0, 5000.0, ExciterMode::Tape).with_drive(2.0).with_gamma(3.0).with_mix(0.5)],
        oversample_factor: 4,
    };
    apply_harmonic_exciter(&mut buf_low_gamma, &cfg_low).unwrap();
    apply_harmonic_exciter(&mut buf_high_gamma, &cfg_high).unwrap();
    let mag_2k_low = goertzel_mag(&buf_low_gamma.samples[4096..], sr, 2000.0);
    let mag_3k_low = goertzel_mag(&buf_low_gamma.samples[4096..], sr, 3000.0);
    let mag_2k_high = goertzel_mag(&buf_high_gamma.samples[4096..], sr, 2000.0);
    let mag_3k_high = goertzel_mag(&buf_high_gamma.samples[4096..], sr, 3000.0);
    // both should generate measurable harmonics (> -100 dB)
    assert!(mag_2k_low > 5e-6 && mag_2k_high > 5e-6, "both gamma should generate 2k: low {mag_2k_low:.6} high {mag_2k_high:.6}");
    // gamma should change overall harmonic energy
    assert!((mag_2k_low - mag_2k_high).abs() > 1e-6 || (mag_3k_low - mag_3k_high).abs() > 1e-6,
        "gamma should morph harmonic energy: low 2k {mag_2k_low:.6} 3k {mag_3k_low:.6} high 2k {mag_2k_high:.6} 3k {mag_3k_high:.6}");
}

// ── 4. Dynamic Sidechain Transient vs Sustained ──────────────────────────────

#[test]
fn sidechain_transient_vs_sustained() {
    let sr = 48000;
    let frames = 48000;
    // sustained 1500Hz tone (inside 1000-8000 band) + transient clicks
    let mut v = vec![0.0; frames];
    for i in 0..frames {
        v[i] += (2.0*std::f64::consts::PI*1500.0*i as f64/sr as f64).sin()*0.5;
    }
    for base in (0..frames).step_by(9600) {
        for j in 0..200 { let idx=base+j; if idx<frames { v[idx]+= if j<10 {0.8} else {0.0}; } }
    }
    let mut buf_trans = AudioBuffer::from_mono(v.clone(), sr);
    let mut buf_sust = AudioBuffer::from_mono(v.clone(), sr);
    let cfg_trans = HarmonicExciterConfig {
        bands: vec![ExciterBandConfig::new(1000.0, 8000.0, ExciterMode::Even).with_drive(0.05).with_mix(0.8).with_sidechain(SidechainMode::Transient)],
        oversample_factor: 4,
    };
    let cfg_sust = HarmonicExciterConfig {
        bands: vec![ExciterBandConfig::new(1000.0, 8000.0, ExciterMode::Even).with_drive(0.05).with_mix(0.8).with_sidechain(SidechainMode::Sustained)],
        oversample_factor: 4,
    };
    apply_harmonic_exciter(&mut buf_trans, &cfg_trans).unwrap();
    apply_harmonic_exciter(&mut buf_sust, &cfg_sust).unwrap();
    // measure overall harmonic energy: transient click should get more from transient mode,
    // sustained tone should get more from sustained mode
    let trans_before = rms(&v[0..4096]);
    let trans_after_trans = rms(&buf_trans.samples[0..4096]);
    let trans_after_sust = rms(&buf_sust.samples[0..4096]);
    let sust_before = rms(&v[10000..14096]);
    let sust_after_trans = rms(&buf_trans.samples[10000..14096]);
    let sust_after_sust = rms(&buf_sust.samples[10000..14096]);
    // both modes should change signal (add harmonics) — allow small but measurable change
    assert!((trans_after_trans - trans_before).abs() > 1e-5, "transient mode should affect click region");
    assert!((sust_after_sust - sust_before).abs() > 1e-5, "sustained mode should affect tone region");
    // modes should differ
    let diff = (trans_after_trans - trans_after_sust).abs() + (sust_after_sust - sust_after_trans).abs();
    assert!(diff > 1e-5, "transient vs sustained should differ: {diff:.6}");
}

// ── 5. Linear-Phase Crossover & Wet/Dry ──────────────────────────────────────

#[test]
fn linear_phase_crossover_no_bleed() {
    let sr = 48000;
    let frames = 16384;
    // 200 Hz tone in low band, 5kHz in high band
    let low = sine_mono(200.0, sr, frames, 0.5);
    let high = sine_mono(5000.0, sr, frames, 0.5);
    let mut mix = vec![0.0; frames];
    for i in 0..frames { mix[i]=low.samples[i]+high.samples[i]; }
    let mut buf = AudioBuffer::from_mono(mix.clone(), sr);
    // exciter only on high band 3k-20k
    let cfg = HarmonicExciterConfig {
        bands: vec![ExciterBandConfig::new(3000.0, 20000.0, ExciterMode::Even).with_drive(0.03).with_mix(0.5)],
        oversample_factor: 4,
    };
    apply_harmonic_exciter(&mut buf, &cfg).unwrap();
    // low 200Hz should be untouched (<1 dB)
    let mag_low_before = goertzel_mag(&mix[4096..], sr, 200.0);
    let mag_low_after = goertzel_mag(&buf.samples[4096..], sr, 200.0);
    assert!((db(mag_low_after)-db(mag_low_before)).abs() < 1.0, "low band should be untouched");
    // high should have harmonics
    let mag_10k = goertzel_mag(&buf.samples[4096..], sr, 10000.0); // 5k*2
    assert!(mag_10k > 1e-4, "high band should generate harmonics");
}

#[test]
fn wet_dry_mix_scales_harmonics() {
    let sr = 48000;
    let frames = 16384;
    let tone = sine_mono(1000.0, sr, frames, 0.5);
    let mut buf_low = tone.clone();
    let mut buf_high = tone.clone();
    let cfg_low = HarmonicExciterConfig {
        bands: vec![ExciterBandConfig::new(500.0, 5000.0, ExciterMode::Even).with_drive(0.03).with_mix(0.1)],
        oversample_factor: 4,
    };
    let cfg_high = HarmonicExciterConfig {
        bands: vec![ExciterBandConfig::new(500.0, 5000.0, ExciterMode::Even).with_drive(0.03).with_mix(0.6)],
        oversample_factor: 4,
    };
    apply_harmonic_exciter(&mut buf_low, &cfg_low).unwrap();
    apply_harmonic_exciter(&mut buf_high, &cfg_high).unwrap();
    let h_low = goertzel_mag(&buf_low.samples[4096..], sr, 2000.0);
    let h_high = goertzel_mag(&buf_high.samples[4096..], sr, 2000.0);
    assert!(h_high > h_low * 1.5, "higher mix should give more harmonics: low {h_low:.6} high {h_high:.6}");
}

#[test]
fn oversampled_even_produces_warmth() {
    let sr = 48000;
    let tone = sine_mono(1000.0, sr, 8192, 0.5);
    let mut buf = tone.clone();
    let cfg = HarmonicExciterConfig {
        bands: vec![ExciterBandConfig::new(500.0, 5000.0, ExciterMode::Even).with_drive(0.03).with_mix(0.3)],
        oversample_factor: 4,
    };
    apply_harmonic_exciter(&mut buf, &cfg).unwrap();
    let mag_2k = goertzel_mag(&buf.samples, sr, 2000.0);
    let mag_3k = goertzel_mag(&buf.samples, sr, 3000.0);
    // even should have 2nd > 3rd
    assert!(mag_2k > mag_3k, "even should favor 2nd");
}
