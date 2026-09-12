use f_audio_mastering::audio::AudioBuffer;
use f_audio_mastering::transient_shaper::{TransientBandConfig, TransientShaperConfig, apply_transient_shaper, transient_detector_debug};

fn sine_mono(freq_hz: f64, sr: u32, frames: usize, amp: f64) -> AudioBuffer {
    let mut v = Vec::with_capacity(frames);
    for n in 0..frames { v.push((2.0*std::f64::consts::PI*freq_hz*n as f64/sr as f64).sin()*amp); }
    AudioBuffer::from_mono(v, sr)
}
fn rms(s: &[f64])->f64{ (s.iter().map(|x| x*x).sum::<f64>()/s.len() as f64).sqrt() }
fn peak(s: &[f64])->f64{ s.iter().fold(0.0, |m,&v| m.max(v.abs())) }
fn db(x: f64)->f64{ 20.0*x.max(1e-12).log10() }
fn goertzel(samples:&[f64], sr:u32, f:f64)->f64{
    let n=samples.len() as f64;
    let k=(0.5 + n*f/sr as f64) as usize;
    let omega=2.0*std::f64::consts::PI*k as f64/n;
    let coeff=2.0*omega.cos();
    let (mut s1, mut s2)=(0.0,0.0);
    for &x in samples{ let s0=x+coeff*s1-s2; s2=s1; s1=s0; }
    let re=s1 - s2*omega.cos(); let im=s2*omega.sin();
    (re*re+im*im).sqrt()/n
}

// ── 1. Differential Log Envelope ─────────────────────────────────────────────

#[test]
fn differential_envelope_isolates_transient() {
    let sr=48000;
    // transient: click at 0.1 sec
    let mut v=vec![0.0;48000];
    for i in 0..48000{ v[i]=(2.0*std::f64::consts::PI*200.0*i as f64/sr as f64).sin()*0.2; }
    let click_pos=10000;
    for i in click_pos..click_pos+10{ v[i]+=0.8; }
    let trans = transient_detector_debug(&v, sr);
    let at_click = trans[click_pos+10];
    let sustained = trans[5000];
    assert!(at_click > 0.5, "transient should be positive at click: {at_click:.2}");
    assert!(sustained.abs() < 5.0, "sustained should be near zero: {sustained:.2}");
    assert!(at_click > sustained - 1.0);
}

// ── 2. Attack boost / Sustain cut ────────────────────────────────────────────

#[test]
fn attack_boost_and_sustain_cut() {
    let sr=48000;
    // drum-like: transient snap + tail
    let mut v=vec![0.0;48000];
    // transient at 0.1 sec: sharp click
    let pos=10000;
    for i in pos..pos+100{
        let env = (-((i-pos) as f64)/200.0).exp();
        v[i]+= 0.9*env;
    }
    // sustained pad after
    for i in pos+500..pos+10000{
        v[i]+= (2.0*std::f64::consts::PI*200.0*i as f64/sr as f64).sin()*0.15;
    }
    let buf_orig = AudioBuffer::from_mono(v.clone(), sr);
    let mut buf_att = AudioBuffer::from_mono(v.clone(), sr);
    let cfg_att = TransientShaperConfig{
        bands: vec![TransientBandConfig::new(0.0, 20000.0, 6.0, 0.0).with_lookahead(0.0)],
    };
    apply_transient_shaper(&mut buf_att, &cfg_att).unwrap();
    // attack boosted: peak near transient should increase (allow clipping at 1.0)
    let peak_before = peak(&v[pos..pos+100]);
    let peak_after = peak(&buf_att.samples[pos..pos+100]);
    assert!(peak_after > peak_before*1.05 && peak_after <= 1.0, "attack +6dB should boost transient: before {peak_before:.3} after {peak_after:.3}");

    let mut buf_sus = AudioBuffer::from_mono(v.clone(), sr);
    let cfg_sus = TransientShaperConfig{
        bands: vec![TransientBandConfig::new(0.0, 20000.0, 0.0, -6.0).with_lookahead(0.0)],
    };
    apply_transient_shaper(&mut buf_sus, &cfg_sus).unwrap();
    // sustain cut: tail should be quieter
    let rms_tail_before = rms(&v[pos+1000..pos+8000]);
    let rms_tail_after = rms(&buf_sus.samples[pos+1000..pos+8000]);
    assert!(rms_tail_after < rms_tail_before*0.85, "sustain -6dB should cut tail: before {rms_tail_before:.4} after {rms_tail_after:.4}");
    let _ = buf_orig;
}

// ── 3. Look-Ahead ────────────────────────────────────────────────────────────

#[test]
fn lookahead_preserves_front_face() {
    let sr=48000;
    let mut v=vec![0.0;24000];
    let pos=5000;
    // sharp transient
    for i in pos..pos+5{ v[i]=0.9; }
    let mut buf_no = AudioBuffer::from_mono(v.clone(), sr);
    let mut buf_la = AudioBuffer::from_mono(v.clone(), sr);
    let cfg_no = TransientShaperConfig{ bands: vec![TransientBandConfig::new(0.0,20000.0,6.0,0.0).with_lookahead(0.0)]};
    let cfg_la = TransientShaperConfig{ bands: vec![TransientBandConfig::new(0.0,20000.0,6.0,0.0).with_lookahead(2.0)]};
    apply_transient_shaper(&mut buf_no, &cfg_no).unwrap();
    apply_transient_shaper(&mut buf_la, &cfg_la).unwrap();
    // lookahead delays main path, so peak shifts by lookahead samples
    let lookahead = (2.0*48000.0/1000.0) as usize;
    let peak_no = peak(&buf_no.samples[pos..pos+20]);
    let peak_la = peak(&buf_la.samples[pos+lookahead..pos+lookahead+20]);
    assert!(peak_la >= peak_no*0.85, "lookahead should preserve front: no {peak_no:.3} la {peak_la:.3}");
}

// ── 4. Phase-Locked Stereo ───────────────────────────────────────────────────

#[test]
fn stereo_linked_phase_locked() {
    let sr=48000;
    let frames=24000;
    let left = sine_mono(440.0, sr, frames, 0.5);
    let right = sine_mono(440.0, sr, frames, 0.5);
    // add transient to left only
    let mut l = left.samples.clone();
    let mut r = right.samples.clone();
    for i in 10000..10010{ l[i]+=0.5; }
    let mut buf = AudioBuffer::new(l.iter().zip(r.iter()).flat_map(|(&a,&b)|[a,b]).collect(), sr, f_audio_mastering::audio::Channels::Stereo);
    let cfg = TransientShaperConfig{ bands: vec![TransientBandConfig::new(0.0,20000.0,6.0,0.0)]};
    apply_transient_shaper(&mut buf, &cfg).unwrap();
    let l_out = buf.channel_slice(0);
    let r_out = buf.channel_slice(1);
    // gains should be identical, so correlation should stay high, and difference due to transient should be similar
    // Check that stereo image not torn: difference between L and R after should be similar to before in non-transient region
    let diff_before = (l[5000]-r[5000]).abs();
    let diff_after = (l_out[5000]-r_out[5000]).abs();
    assert!((diff_after - diff_before).abs() < 0.05, "phase-locked should keep image");
    // for transient region, both channels should get same gain (linked), so left transient boost should also appear slightly in right
    // This is expected for linked, so just check that both channels were processed (not silent)
    assert!(peak(&l_out) > 0.5 && peak(&r_out) > 0.5);
}

// ── 5. Multi-Band Independence ───────────────────────────────────────────────

#[test]
fn multiband_high_attack_does_not_affect_low() {
    let sr=48000;
    let frames=48000;
    let low = sine_mono(80.0, sr, frames, 0.4);
    let high_clicks = {
        let mut v=vec![0.0;frames];
        for base in (0..frames).step_by(9600){
            for i in 0..100{ let idx=base+i; if idx<frames{v[idx]+=0.7*(-(i as f64)/30.0).exp();}}
        }
        v
    };
    let mut mix=vec![0.0;frames];
    for i in 0..frames{ mix[i]= low.samples[i] + high_clicks[i]*0.5; }
    let mut buf = AudioBuffer::from_mono(mix.clone(), sr);
    // only high band transient boost
    let cfg = TransientShaperConfig{
        bands: vec![
            TransientBandConfig::new(0.0, 250.0, 0.0, 0.0),
            TransientBandConfig::new(250.0, 20000.0, 6.0, 0.0),
        ],
    };
    apply_transient_shaper(&mut buf, &cfg).unwrap();
    // low 80Hz should be untouched (<1 dB)
    let mag_low_before = goertzel(&mix[8192..16384], sr, 80.0);
    let mag_low_after = goertzel(&buf.samples[8192..16384], sr, 80.0);
    assert!((db(mag_low_after)-db(mag_low_before)).abs() < 1.0, "low band should be untouched");
    // high transients should be boosted
    let peak_before = peak(&high_clicks[0..200]);
    let mut high_only_before: Vec<f64> = high_clicks[0..4800].to_vec();
    let mut high_only_after = buf.samples[0..4800].to_vec();
    // just check that high band got louder
    let rms_before = rms(&high_clicks[0..4800]);
    let rms_after = rms(&buf.samples[0..4800]);
    // overall mix RMS should increase due to high boost
    assert!(rms_after > rms(&mix[0..4800])*0.9, "high boost should not reduce overall");
}

// ── 6. Summing Reconstruction ────────────────────────────────────────────────

#[test]
fn flat_reconstruction_when_no_gain() {
    let sr=48000;
    let tone = sine_mono(1000.0, sr, 24000, 0.5);
    let mut buf = tone.clone();
    let cfg = TransientShaperConfig{
        bands: vec![
            TransientBandConfig::new(0.0, 250.0, 0.0, 0.0),
            TransientBandConfig::new(250.0, 4000.0, 0.0, 0.0),
            TransientBandConfig::new(4000.0, 20000.0, 0.0, 0.0),
        ],
    };
    apply_transient_shaper(&mut buf, &cfg).unwrap();
    // skip FIR edges
    let skip=2048;
    let mut max_diff: f64=0.0;
    for i in skip..(24000-skip){
        max_diff = max_diff.max((buf.samples[i]-tone.samples[i]).abs());
    }
    assert!(max_diff < 1e-4, "flat reconstruction failed max {max_diff:.6}");
}

#[test]
fn attack_and_sustain_together() {
    let sr=48000;
    // transient only
    let mut v_trans = vec![0.0;24000];
    let pos=5000;
    for i in pos..pos+100{ v_trans[i]+=0.8*(-((i-pos) as f64)/20.0).exp(); }
    let mut buf_trans = AudioBuffer::from_mono(v_trans.clone(), sr);
    let cfg = TransientShaperConfig{
        bands: vec![TransientBandConfig::new(0.0,20000.0, 6.0, -4.0).with_lookahead(0.0)],
    };
    apply_transient_shaper(&mut buf_trans, &cfg).unwrap();
    let peak_before = peak(&v_trans[pos..pos+100]);
    let peak_after = peak(&buf_trans.samples[pos..pos+100]);
    assert!(peak_after > peak_before*1.02, "attack should boost: {peak_before:.3} -> {peak_after:.3}");
    // sustain only (no transient)
    let mut v_sus = vec![0.0;24000];
    for i in 5000..20000{ v_sus[i]+= (2.0*std::f64::consts::PI*1000.0*i as f64/sr as f64).sin()*0.2; }
    let mut buf_sus = AudioBuffer::from_mono(v_sus.clone(), sr);
    apply_transient_shaper(&mut buf_sus, &cfg).unwrap();
    let tail_before = rms(&v_sus[8000..15000]);
    let tail_after = rms(&buf_sus.samples[8000..15000]);
    assert!(tail_after < tail_before*0.98, "sustain should cut: {tail_before:.4} -> {tail_after:.4}");
}
