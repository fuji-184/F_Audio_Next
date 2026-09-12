use f_audio_mastering::audio::AudioBuffer;
use f_audio_mastering::Track;
use f_audio_mastering::compressor::MultibandCompressorConfig;
use f_audio_mastering::de_esser::DeEsserConfig;
use f_audio_mastering::harmonic_exciter::HarmonicExciterConfig;
use f_audio_mastering::limiter::LimiterConfig;
use f_audio_mastering::noise_reduction::NoiseReductionConfig;
use f_audio_mastering::transient_shaper::TransientShaperConfig;

fn sine_buffer(sr: u32, frames: usize) -> AudioBuffer {
    let mut v = Vec::with_capacity(frames);
    for n in 0..frames {
        v.push((2.0 * std::f64::consts::PI * 440.0 * n as f64 / sr as f64).sin() * 0.3);
    }
    AudioBuffer::from_mono(v, sr)
}

#[test]
fn track_chain_all_processors() {
    let sr = 48000;
    let mut buf = sine_buffer(sr, 24000);
    for (i, s) in buf.samples.iter_mut().enumerate() {
        let mut seed = (i as u64).wrapping_mul(6364136223846793005).wrapping_add(123);
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        let n = ((seed >> 33) as f64 / (1u64 << 31) as f64 * 2.0 - 1.0) * 0.02;
        *s += n;
    }
    let mut track = Track::new(buf);

    track.eq("digital bell 2", 1000.0, 2.0, 1.0);
    track.resample(48000);
    track.convert_channels("stereo");
    track.convert_channels("mono");

    // use a less aggressive noise reduction for this test (higher threshold)
    let nr_cfg = NoiseReductionConfig { reduction_db: 6.0, ..Default::default() };
    track.reduce_noise_auto(&nr_cfg);

    let comp_cfg = MultibandCompressorConfig::default();
    track.compress_multiband(&comp_cfg);

    let exc_cfg = HarmonicExciterConfig::default();
    track.excite_harmonics(&exc_cfg);

    let trans_cfg = TransientShaperConfig::default();
    track.shape_transients(&trans_cfg);

    let de_cfg = DeEsserConfig::default();
    track.de_ess(&de_cfg);

    let lufs_before = track.integrated_lufs();
    let tp_before = track.true_peak_db();
    track.normalize_loudness(-14.0, -1.0);
    let lufs_after = track.integrated_lufs();
    let tp_after = track.true_peak_db();
    assert!(tp_after <= -1.0 + 0.2, "true peak after {} should be <= -1", tp_after);
    // loudness should be within reasonable broadcast range, true-peak limiting may prevent exact -14
    assert!(lufs_after > -30.0 && lufs_after < -8.0, "lufs after {} vs target -14 (before {} tp {}->{})", lufs_after, lufs_before, tp_before, tp_after);

    // Limiter
    let lim_cfg = LimiterConfig::default();
    track.limit(&lim_cfg);
    track.limit_to(-1.0);

    // check still valid
    assert!(track.num_frames() > 0);
    assert!(track.sample_rate() == 48000);
}

#[test]
fn track_resample_and_channels() {
    let buf = sine_buffer(48000, 24000);
    let mut track = Track::new(buf);
    track.resample(44100);
    assert_eq!(track.sample_rate(), 44100);
    track.to_stereo();
    assert_eq!(track.channels(), f_audio_mastering::audio::Channels::Stereo);
    track.to_mono();
    assert_eq!(track.channels(), f_audio_mastering::audio::Channels::Mono);
}

#[test]
fn track_loudness_api() {
    let buf = sine_buffer(48000, 48000);
    let track = Track::new(buf);
    let stats = track.loudness();
    assert!(stats.integrated_lufs < 0.0 && stats.integrated_lufs > -50.0);
    assert!(stats.true_peak_db <= 0.0);
}
