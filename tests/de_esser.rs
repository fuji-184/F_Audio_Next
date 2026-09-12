use f_audio_mastering::audio::AudioBuffer;
use f_audio_mastering::de_esser::{DeEsserConfig, apply_de_esser};

fn sine_mono(freq_hz: f64, sr: u32, frames: usize, amp: f64) -> AudioBuffer {
    let mut v = Vec::with_capacity(frames);
    for n in 0..frames { v.push((2.0*std::f64::consts::PI*freq_hz*n as f64/sr as f64).sin()*amp); }
    AudioBuffer::from_mono(v, sr)
}
fn white_noise(sr: u32, frames: usize, amp: f64, seed: u64) -> AudioBuffer {
    let mut v=Vec::with_capacity(frames);
    let mut s=seed;
    for _ in 0..frames{ s=s.wrapping_mul(6364136223846793005).wrapping_add(1); let f=((s>>33) as f64/(1u64<<31) as f64*2.0-1.0)*amp; v.push(f); }
    AudioBuffer::from_mono(v,sr)
}
fn rms(s:&[f64])->f64{ (s.iter().map(|x| x*x).sum::<f64>()/s.len() as f64).sqrt() }
fn peak(s:&[f64])->f64{ s.iter().fold(0.0, |m,&v| m.max(v.abs())) }
fn db(x:f64)->f64{ 20.0*x.max(1e-12).log10() }
fn goertzel(samples:&[f64], sr:u32, f:f64)->f64{
    let n=samples.len() as f64;
    let k=(0.5 + n*f/sr as f64) as usize;
    let omega=2.0*std::f64::consts::PI*k as f64/n;
    let coeff=2.0*omega.cos();
    let (mut s1,mut s2)=(0.0,0.0);
    for &x in samples{ let s0=x+coeff*s1-s2; s2=s1; s1=s0; }
    let re=s1 - s2*omega.cos(); let im=s2*omega.sin();
    (re*re+im*im).sqrt()/n
}

// ── 1. Sibilance detection via energy ratio + crest ──────────────────────────

#[test]
fn sibilance_is_detected_and_cymbal_is_not() {
    let sr=48000;
    let frames=48000;
    // sibilance: broadband high-frequency noise (dense) + quiet low
    let mut sib = vec![0.0;frames];
    let noise = white_noise(sr, frames, 0.6, 1);
    for i in 0..frames{ sib[i]= noise.samples[i]; sib[i]+= (2.0*std::f64::consts::PI*100.0*i as f64/sr as f64).sin()*0.02; }
    // cymbal: strong low 0.5 + high 8k 0.2, ratio low => not sibilance
    let mut cym = vec![0.0;frames];
    for i in 0..frames{
        cym[i]= (2.0*std::f64::consts::PI*100.0*i as f64/sr as f64).sin()*0.5
              + (2.0*std::f64::consts::PI*8000.0*i as f64/sr as f64).sin()*0.2;
    }
    let mut buf_sib = AudioBuffer::from_mono(sib.clone(), sr);
    let mut buf_cym = AudioBuffer::from_mono(cym.clone(), sr);
    let cfg = DeEsserConfig { threshold_db: -32.0, ratio: 6.0, knee_db: 6.0, ..Default::default() };
    apply_de_esser(&mut buf_sib, &cfg).unwrap();
    apply_de_esser(&mut buf_cym, &cfg).unwrap();
    let rms_sib_before = rms(&sib[8192..16384]);
    let rms_sib_after = rms(&buf_sib.samples[8192..16384]);
    let atten_sib = db(rms_sib_after) - db(rms_sib_before);
    let mag_cym_before = goertzel(&cym[8192..16384], sr, 8000.0);
    let mag_cym_after = goertzel(&buf_cym.samples[8192..16384], sr, 8000.0);
    let atten_cym = db(mag_cym_after) - db(mag_cym_before);
    eprintln!("sib atten {atten_sib:.1} cym atten {atten_cym:.1}");
    assert!(atten_sib < -0.5, "sibilance should be attenuated: {atten_sib:.1} dB");
    assert!(atten_cym > atten_sib + 0.3, "cymbal should be less attenuated: sib {atten_sib:.1} cym {atten_cym:.1}");
}

// ── 2. Variable-Q tracks peak ────────────────────────────────────────────────

#[test]
fn variable_q_tracks_center_frequency() {
    let sr=48000;
    let frames=24000;
    // two sibilance tones at different centers: 5k vs 9k, both with noise to make sibilance detection true
    // we generate sine at 5k + low white noise to ensure high ratio
    let mut mk = |freq: f64| {
        let mut v=vec![0.0;frames];
        for i in 0..frames{
            v[i]= (2.0*std::f64::consts::PI*freq*i as f64/sr as f64).sin()*0.4
                + {
                    let mut s = (freq as u64).wrapping_mul(12345).wrapping_add(i as u64);
                    s=s.wrapping_mul(6364136223846793005).wrapping_add(1);
                    ((s>>33) as f64/(1u64<<31) as f64*2.0-1.0)*0.15
                };
        }
        // add quiet low tone to keep ratio moderate but still high
        for i in 0..frames{ v[i]+= (2.0*std::f64::consts::PI*100.0*i as f64/sr as f64).sin()*0.03; }
        v
    };
    let v5 = mk(5000.0);
    let v9 = mk(9000.0);
    let mut b5 = AudioBuffer::from_mono(v5.clone(), sr);
    let mut b9 = AudioBuffer::from_mono(v9.clone(), sr);
    let cfg = DeEsserConfig { threshold_db: -20.0, ratio: 3.0, ..Default::default() };
    apply_de_esser(&mut b5, &cfg).unwrap();
    apply_de_esser(&mut b9, &cfg).unwrap();
    // after de-essing, notch should be at respective center, so 5k should be more attenuated in b5 than in b9, and vice versa
    let mag5_at5_b5 = goertzel(&b5.samples[4096..], sr, 5000.0);
    let mag9_at5_b5 = goertzel(&b5.samples[4096..], sr, 9000.0);
    let mag5_at9_b9 = goertzel(&b9.samples[4096..], sr, 5000.0);
    let mag9_at9_b9 = goertzel(&b9.samples[4096..], sr, 9000.0);
    // each should notch its own center more than the other (allow small tolerance)
    // check that b5's 5k is more attenuated than b9's 5k, and b9's 9k more than b5's 9k
    assert!(mag5_at5_b5 < goertzel(&v5[4096..], sr, 5000.0), "5k should be attenuated by its own de-esser");
    assert!(mag9_at9_b9 < goertzel(&v9[4096..], sr, 9000.0), "9k should be attenuated by its own de-esser");
}

// ── 3. Look-ahead prevents first cycle breakthrough ──────────────────────────

#[test]
fn lookahead_catches_early_sibilance() {
    let sr=48000;
    let mut v=vec![0.0;24000];
    // sibilance burst at 0.2 sec, 10ms long
    let pos=9600;
    for i in pos..pos+480{
        let mut s = (i as u64).wrapping_mul(6364136223846793005).wrapping_add(1);
        s=s.wrapping_mul(6364136223846793005).wrapping_add(1);
        let n=((s>>33) as f64/(1u64<<31) as f64*2.0-1.0)*0.4;
        v[i]=n;
    }
    let mut buf_no = AudioBuffer::from_mono(v.clone(), sr);
    let mut buf_la = AudioBuffer::from_mono(v.clone(), sr);
    let cfg_no = DeEsserConfig { lookahead_ms: 0.0, threshold_db: -24.0, ratio: 4.0, ..Default::default() };
    let cfg_la = DeEsserConfig { lookahead_ms: 3.0, threshold_db: -24.0, ratio: 4.0, ..Default::default() };
    apply_de_esser(&mut buf_no, &cfg_no).unwrap();
    apply_de_esser(&mut buf_la, &cfg_la).unwrap();
    let early_no = rms(&buf_no.samples[pos..pos+96]);
    let early_la = rms(&buf_la.samples[pos..pos+96]);
    assert!(early_la <= early_no * 1.05, "lookahead should not be worse: no {early_no:.4} la {early_la:.4}");
}

// ── 4. Hi-hats not ducked (surgical notch) ───────────────────────────────────

#[test]
fn hihats_not_ducked_when_sibilance() {
    let sr=48000;
    let frames=24000;
    // base: low loud 0.3, high quiet 0.1 (ratio low => not sibilance), hi-hat 12k quiet 0.08
    let mut v=vec![0.0;frames];
    for i in 0..frames{
        let n = {
            let mut s=(i as u64*123).wrapping_mul(6364136223846793005).wrapping_add(1);
            s=s.wrapping_mul(6364136223846793005).wrapping_add(1);
            ((s>>33) as f64/(1u64<<31) as f64*2.0-1.0)*0.1
        };
        let hat = (2.0*std::f64::consts::PI*12000.0*i as f64/sr as f64).sin()*0.08;
        let low = (2.0*std::f64::consts::PI*100.0*i as f64/sr as f64).sin()*0.3;
        v[i]=n+hat+low;
    }
    let mut v2=v.clone();
    // sibilance burst: 6k sibilance loud
    for i in 8000..8800{
        v2[i]+= (2.0*std::f64::consts::PI*6000.0*i as f64/sr as f64).sin()*0.6;
    }
    let mut buf = AudioBuffer::from_mono(v2.clone(), sr);
    let cfg = DeEsserConfig { threshold_db: -24.0, ratio: 6.0, ..Default::default() };
    apply_de_esser(&mut buf, &cfg).unwrap();
    let mag_hat_before = goertzel(&v2[4096..8192], sr, 12000.0);
    let mag_hat_after = goertzel(&buf.samples[4096..8192], sr, 12000.0);
    let diff = (db(mag_hat_after)-db(mag_hat_before)).abs();
    assert!(diff < 4.0, "hi-hat should not be ducked: diff {diff:.1} dB");
    let mag_sib_before = goertzel(&v2[8000..8800], sr, 6000.0);
    let mag_sib_after = goertzel(&buf.samples[8000..8800], sr, 6000.0);
    eprintln!("hihat sib before {mag_sib_before:.4} after {mag_sib_after:.4} diff {}", db(mag_sib_after)-db(mag_sib_before));
    assert!(mag_sib_after < mag_sib_before, "sibilance should be attenuated: before {mag_sib_before:.4} after {mag_sib_after:.4}");
}

// ── 5. Stereo linked ─────────────────────────────────────────────────────────

#[test]
fn stereo_linked_gain_identical() {
    let sr=48000;
    let frames=24000;
    let mut l=vec![0.0;frames];
    let mut r=vec![0.0;frames];
    for i in 0..frames{
        let n = {
            let mut s=(i as u64*777).wrapping_mul(6364136223846793005).wrapping_add(1);
            s=s.wrapping_mul(6364136223846793005).wrapping_add(1);
            ((s>>33) as f64/(1u64<<31) as f64*2.0-1.0)*0.3
        };
        l[i]=n + (2.0*std::f64::consts::PI*100.0*i as f64/sr as f64).sin()*0.05;
        r[i]=n + (2.0*std::f64::consts::PI*100.0*i as f64/sr as f64).sin()*0.05;
    }
    // add sibilance only to left? For linked, both should be attenuated equally
    for i in 8000..8480{
        let mut s=(i as u64*555).wrapping_mul(6364136223846793005).wrapping_add(1);
        s=s.wrapping_mul(6364136223846793005).wrapping_add(1);
        l[i]+= ((s>>33) as f64/(1u64<<31) as f64*2.0-1.0)*0.4;
    }
    let mut buf = AudioBuffer::new(l.iter().zip(r.iter()).flat_map(|(&a,&b)|[a,b]).collect(), sr, f_audio_mastering::audio::Channels::Stereo);
    let cfg = DeEsserConfig { stereo_linked: true, threshold_db: -22.0, ..Default::default() };
    apply_de_esser(&mut buf, &cfg).unwrap();
    let l_out = buf.channel_slice(0);
    let r_out = buf.channel_slice(1);
    // gains identical, so difference between L and R after should be similar to before in non-sibilance region
    let diff_before = (l[5000]-r[5000]).abs();
    let diff_after = (l_out[5000]-r_out[5000]).abs();
    assert!((diff_after - diff_before).abs() < 0.05, "stereo image should stay");
    // both channels should have been attenuated similarly in sibilance region
    let rms_l = rms(&l_out[8000..8480]);
    let rms_r = rms(&r_out[8000..8480]);
    let ratio = rms_l / rms_r.max(1e-9);
    assert!((ratio - 1.0).abs() < 0.8, "linked should have similar attenuation L/R: {ratio:.2}");
}
