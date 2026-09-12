use f_audio_mastering::try_read;
use f_audio_mastering::compressor::{CompressorBandConfig, MultibandCompressorConfig};
use f_audio_mastering::de_esser::DeEsserConfig;
use f_audio_mastering::harmonic_exciter::{ExciterBandConfig, HarmonicExciterConfig, ExciterMode};
use f_audio_mastering::limiter::{LimiterConfig, LimiterStyle};
use f_audio_mastering::transient_shaper::TransientBandConfig;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let input_path = args.get(1).map(String::as_str).unwrap_or("./../../Feel.mp3");
    let output_path = args.get(2).map(String::as_str).unwrap_or("./output");

    println!("Reading: {}", input_path);
    let mut track = try_read(input_path).unwrap_or_else(|e| {
        eprintln!("Error: {}", e);
        std::process::exit(1);
    });
    println!(
        "Loaded: {} Hz, {:?}, {:.2}s, loudness {:.1} LUFS, true-peak {:.1} dBTP",
        track.sample_rate(),
        track.channels(),
        track.duration_seconds(),
        track.integrated_lufs(),
        track.true_peak_db()
    );

    // ── Mastering chain for AI-generated music ───────────────────────────
    // AI renders often have slightly harsh mids and uncontrolled dynamics.
    // Chain: corrective EQ → gentle glue compression → transient punch → de-ess → subtle warmth → loudness → brickwall

    track
        // 1) Corrective EQ (same as before, surgical)
        .eq("low cut 1", 30.0, 0.0, 1.40)
        .eq("digital bell", 150.0, -5.5, 1.40)
        .eq("digital bell", 350.0, -2.0, 1.40)
        .eq("high cut 1", 16000.0, 0.0, 1.40)
        .eq("digital bell 2", 4000.0, -1.5, 3.0)
        .eq("digital bell", 2500.0, -4.0, 3.0)
        .eq("high shelf", 3200.0, -5.0, 1.40);

    // 2) Glue compression – single broadband gentle 2:1, -14dB thresh
    let comp = MultibandCompressorConfig {
        bands: vec![
            CompressorBandConfig::new(0.0, 16000.0, -14.0, 2.0)
                .with_attack(15.0)
                .with_release(120.0)
                .with_knee(6.0),
        ],
        lookahead_ms: 2.0,
        auto_release: true,
    };
    track.compress_multiband(&comp);
    println!("After glue comp: {:.1} LUFS", track.integrated_lufs());

    // 3) Transient punch – add snap on mids/highs, keep low tight
    let trans_cfg = f_audio_mastering::transient_shaper::TransientShaperConfig {
        bands: vec![
            TransientBandConfig::new(0.0, 250.0, 1.0, -1.0).with_lookahead(1.5),
            TransientBandConfig::new(250.0, 6000.0, 2.5, -1.5).with_lookahead(2.0),
        ],
    };
    track.shape_transients(&trans_cfg);

    // 4) De-ess – taming AI sibilance 5-9kHz
    let de_cfg = DeEsserConfig {
        low_hz: 5000.0,
        high_hz: 9000.0,
        threshold_db: -18.0,
        ratio: 3.0,
        knee_db: 6.0,
        lookahead_ms: 2.0,
        stereo_linked: true,
    };
    track.de_ess(&de_cfg);

    // 5) Harmonic exciter – subtle triode warmth 2-6kHz, tape glue on top
    let exc_cfg = HarmonicExciterConfig {
        bands: vec![
            ExciterBandConfig::new(2000.0, 6000.0, ExciterMode::Even)
                .with_drive(0.015)
                .with_mix(0.18),
        ],
        oversample_factor: 4,
    };
    track.excite_harmonics(&exc_cfg);

    // 6) Loudness normalization to streaming standard
    let before_lufs = track.integrated_lufs();
    let before_tp = track.true_peak_db();
    track.normalize_loudness(-14.0, -1.0);
    println!(
        "Loudness: {:.1} LUFS ({:.1} dBTP) -> {:.1} LUFS ({:.1} dBTP)",
        before_lufs,
        before_tp,
        track.integrated_lufs(),
        track.true_peak_db()
    );

    // 7) Brickwall limiter – final ceiling -1.0 dBTP, transparent
    let lim_cfg = LimiterConfig {
        ceiling_db: -1.0,
        lookahead_ms: 2.0,
        oversample_factor: 4,
        style: LimiterStyle::Transparent,
    };
    track.limit(&lim_cfg);
    println!(
        "After limiter: {:.1} LUFS, true-peak {:.1} dBTP",
        track.integrated_lufs(),
        track.true_peak_db()
    );

    // ── Save FLAC 48kHz stereo 24-bit (master) ───────────────────────────
    track
        .save_flac(output_path, 48000, "stereo", 24)
        .unwrap_or_else(|e| {
            eprintln!("Error saving FLAC: {}", e);
            std::process::exit(1);
        });
    println!("Saved → {}.flac (48kHz stereo 24-bit)", output_path);
    println!("Done. Loudness {:.1} LUFS, peak {:.1} dBTP", track.integrated_lufs(), track.true_peak_db());
}
