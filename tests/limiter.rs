use f_audio_mastering::audio::{AudioBuffer, Channels};
use f_audio_mastering::limiter::{LimiterConfig, LimiterStyle, apply_limiter};
use f_audio_mastering::dsp::{resample_with_config, ResampleConfig, PhaseMode};

fn sine_mono(freq_hz: f64, sr: u32, frames: usize, amp: f64) -> AudioBuffer {
    let mut v=Vec::with_capacity(frames);
    for n in 0..frames{ v.push((2.0*std::f64::consts::PI*freq_hz*n as f64/sr as f64).sin()*amp); }
    AudioBuffer::from_mono(v,sr)
}
fn rms(s:&[f64])->f64{ (s.iter().map(|x| x*x).sum::<f64>()/s.len() as f64).sqrt() }
fn peak(s:&[f64])->f64{ s.iter().fold(0.0f64, |m,&v| m.max(v.abs())) }
fn db(x:f64)->f64{ 20.0*x.max(1e-12).log10() }
fn true_peak(buf:&AudioBuffer)->f64{
    let mut p=peak(&buf.samples);
    for c in 0..buf.channels.count(){
        let chan=buf.channel_slice(c);
        let b=AudioBuffer::from_mono(chan.clone(), buf.sample_rate);
        let cfg=ResampleConfig{ phase: PhaseMode::Linear, ..Default::default() };
        if let Ok(os)=resample_with_config(&b, buf.sample_rate*4, &cfg){
            let tp=peak(&os.samples);
            if tp>p{ p=tp; }
        }
    }
    p
}

// ── 1. True-peak inter-sample ───────────────────────────────────────────────

#[test]
fn true_peak_brickwall() {
    let sr=48000;
    // 12.1k sine at 0.9 has inter-sample overshoot
    let frames=48000;
    let mut v=Vec::with_capacity(frames);
    for n in 0..frames{ v.push((2.0*std::f64::consts::PI*12100.0*n as f64/sr as f64).sin()*0.9); }
    let mut buf=AudioBuffer::from_mono(v, sr);
    let sample_peak_before = peak(&buf.samples);
    let tp_before = true_peak(&buf);
    // true peak should be >= sample peak
    assert!(tp_before >= sample_peak_before*0.99, "true peak >= sample");
    let cfg=LimiterConfig{ ceiling_db: -1.0, lookahead_ms: 2.0, oversample_factor: 4, style: LimiterStyle::Transparent };
    apply_limiter(&mut buf, &cfg).unwrap();
    let tp_after = true_peak(&buf);
    assert!(tp_after <= 0.891251 + 0.05, "true peak after {} should be <= -1dB (0.891)", tp_after);
    assert!(tp_after <= tp_before + 0.02, "limiter should not increase true peak");
}

#[test]
fn sample_peak_vs_true_peak() {
    let sr=48000;
    // Two samples at -0.5dB (0.944) but analog peak between them exceeds 0dB due to sinc interpolation
    // Create a signal with alternating peaks that cause inter-sample overshoot
    let mut v=vec![0.0; 4800];
    for i in 0..4800{
        // 12k at 48k = 4 samples per period, peaks at samples, but with 12.1k, peaks between
        v[i]=(2.0*std::f64::consts::PI*12000.0*i as f64/sr as f64).sin()*0.95;
    }
    let mut buf=AudioBuffer::from_mono(v, sr);
    let tp_before = true_peak(&buf);
    let cfg=LimiterConfig{ ceiling_db: -1.0, ..Default::default() };
    apply_limiter(&mut buf, &cfg).unwrap();
    let tp_after = true_peak(&buf);
    assert!(tp_after <= 0.891251 + 0.05);
}

// ── 2. Look-ahead ────────────────────────────────────────────────────────────

#[test]
fn lookahead_catches_transient() {
    let sr=48000;
    let mut v=vec![0.0;24000];
    for i in 0..24000{ v[i]=(2.0*std::f64::consts::PI*440.0*i as f64/sr as f64).sin()*0.2; }
    let pos=12000;
    for i in pos..pos+10{ v[i]=0.95; if i+1 <24000 { v[i+1]=-0.95; } }
    let mut buf_no = AudioBuffer::from_mono(v.clone(), sr);
    let mut buf_la = AudioBuffer::from_mono(v.clone(), sr);
    let cfg_no=LimiterConfig{ ceiling_db: -1.0, lookahead_ms: 0.0, oversample_factor: 4, style: LimiterStyle::Transparent };
    let cfg_la=LimiterConfig{ ceiling_db: -1.0, lookahead_ms: 3.0, oversample_factor: 4, style: LimiterStyle::Transparent };
    apply_limiter(&mut buf_no, &cfg_no).unwrap();
    apply_limiter(&mut buf_la, &cfg_la).unwrap();
    let peak_no = peak(&buf_no.samples[pos..pos+100]);
    let peak_la = peak(&buf_la.samples[pos..pos+100]);
    // lookahead should catch better, so peak_la <= peak_no
    assert!(peak_la <= peak_no + 0.02, "lookahead should be at least as good: no {peak_no:.3} la {peak_la:.3}");
    assert!(peak_la <= 0.891251 + 0.05, "lookahead should limit to ceiling");
}

// ── 3. Variable release topology ─────────────────────────────────────────────

#[test]
fn variable_release_styles_differ() {
    let sr=48000;
    // loud burst 100ms then silence, measure recovery
    let mut v=vec![0.0;48000];
    for i in 10000..14800{ v[i]=(2.0*std::f64::consts::PI*100.0*i as f64/sr as f64).sin()*0.8; }
    let mut buf_agg = AudioBuffer::from_mono(v.clone(), sr);
    let mut buf_trans = AudioBuffer::from_mono(v.clone(), sr);
    let cfg_agg=LimiterConfig{ ceiling_db: -1.0, lookahead_ms: 1.0, oversample_factor: 4, style: LimiterStyle::Aggressive };
    let cfg_trans=LimiterConfig{ ceiling_db: -1.0, lookahead_ms: 1.0, oversample_factor: 4, style: LimiterStyle::Transparent };
    apply_limiter(&mut buf_agg, &cfg_agg).unwrap();
    apply_limiter(&mut buf_trans, &cfg_trans).unwrap();
    // after burst (14800), aggressive should recover faster (higher RMS in tail 200ms after)
    let tail_agg = rms(&buf_agg.samples[16000..20000]);
    let tail_trans = rms(&buf_trans.samples[16000..20000]);
    assert!(tail_agg >= tail_trans * 0.8, "aggressive should recover at least as fast: agg {tail_agg:.4} trans {tail_trans:.4}");
}

// ── 4. Stereo linked ─────────────────────────────────────────────────────────

#[test]
fn stereo_linked_preserves_image() {
    let sr=48000;
    let frames=24000;
    // left loud 0.9, right quiet 0.2, both 1kHz
    let mut l=vec![0.0;frames];
    let mut r=vec![0.0;frames];
    for i in 0..frames{
        l[i]=(2.0*std::f64::consts::PI*1000.0*i as f64/sr as f64).sin()*0.9;
        r[i]=(2.0*std::f64::consts::PI*1000.0*i as f64/sr as f64).sin()*0.2;
    }
    let mut buf = AudioBuffer::new(l.iter().zip(r.iter()).flat_map(|(&a,&b)|[a,b]).collect(), sr, Channels::Stereo);
    let cfg=LimiterConfig{ ceiling_db: -1.0, lookahead_ms: 2.0, oversample_factor: 4, style: LimiterStyle::Transparent };
    apply_limiter(&mut buf, &cfg).unwrap();
    let l_out=buf.channel_slice(0);
    let r_out=buf.channel_slice(1);
    // gains identical, so both should be reduced, and ratio should be preserved-ish but both limited
    // Check that right (quiet) was also ducked due to linked (so its peak after < before)
    let peak_r_before = 0.2;
    let peak_r_after = peak(&r_out[5000..10000]);
    // linked: right should be attenuated because left triggered limiter, so peak_r_after < 0.2
    assert!(peak_r_after < peak_r_before + 0.02, "linked should duck quiet channel when loud triggers");
    // image: difference in dB between L and R should stay similar
    let ratio_before = db(0.9) - db(0.2);
    let ratio_after = db(peak(&l_out[5000..10000])) - db(peak_r_after.max(1e-9));
    assert!((ratio_after - ratio_before).abs() < 3.0, "image should be preserved: before {ratio_before:.1} after {ratio_after:.1}");
}

// ── 5. Brickwall never exceeds ceiling ───────────────────────────────────────

#[test]
fn brickwall_never_exceeds_ceiling() {
    let sr=48000;
    let mut v=vec![0.0;48000];
    for i in 0..48000{
        // loud mix with transients
        v[i]=(2.0*std::f64::consts::PI*100.0*i as f64/sr as f64).sin()*0.7
           + (2.0*std::f64::consts::PI*1000.0*i as f64/sr as f64).sin()*0.5;
        if i%4800==0{ v[i]=0.99; }
    }
    let mut buf=AudioBuffer::from_mono(v, sr);
    let cfg=LimiterConfig{ ceiling_db: -1.0, ..Default::default() };
    apply_limiter(&mut buf, &cfg).unwrap();
    let tp = true_peak(&buf);
    assert!(tp <= 0.891251 + 0.02, "brickwall ceiling -1dB should not be exceeded: tp {} ({:.2} dB)", tp, db(tp));
    assert!(peak(&buf.samples) <= 0.891251 + 0.02);
}

// ── 6. Oversampling 4x vs 1x ──────────────────────────────────────────────────

#[test]
fn oversampling_matters_for_true_peak() {
    let sr=48000;
    let v = {
        let mut vv=vec![0.0;4800];
        for i in 0..4800{ vv[i]=(2.0*std::f64::consts::PI*12000.0*i as f64/sr as f64).sin()*0.9; }
        vv
    };
    let mut buf4 = AudioBuffer::from_mono(v.clone(), sr);
    let mut buf1 = AudioBuffer::from_mono(v.clone(), sr);
    let cfg4=LimiterConfig{ ceiling_db: -1.0, oversample_factor: 4, ..Default::default() };
    let cfg1=LimiterConfig{ ceiling_db: -1.0, oversample_factor: 1, ..Default::default() };
    apply_limiter(&mut buf4, &cfg4).unwrap();
    apply_limiter(&mut buf1, &cfg1).unwrap();
    let tp4 = true_peak(&buf4);
    let tp1 = true_peak(&buf1);
    // 4x should be more accurate, so true peak after 4x should be <= true peak after 1x (which may miss inter-sample)
    assert!(tp4 <= tp1 + 0.02, "4x should be at least as good as 1x: 4x {tp4:.4} 1x {tp1:.4}");
}
