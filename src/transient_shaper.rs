use crate::audio::AudioBuffer;
use crate::error::Result;
use std::f64::consts::PI;

// ── config ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct TransientBandConfig {
    pub low_hz: f64,
    pub high_hz: f64,
    /// extra gain applied to transient part (positive = punchier, e.g. +6 dB)
    pub attack_gain_db: f64,
    /// gain applied to sustain part (negative = tighter, e.g. -3 dB)
    pub sustain_gain_db: f64,
    pub lookahead_ms: f64,
}

impl TransientBandConfig {
    pub fn new(low_hz: f64, high_hz: f64, attack_db: f64, sustain_db: f64) -> Self {
        Self { low_hz, high_hz, attack_gain_db: attack_db, sustain_gain_db: sustain_db, lookahead_ms: 2.0 }
    }
    pub fn with_lookahead(mut self, ms: f64) -> Self { self.lookahead_ms = ms.clamp(0.0, 3.0); self }
}

#[derive(Debug, Clone)]
pub struct TransientShaperConfig {
    pub bands: Vec<TransientBandConfig>,
}

impl Default for TransientShaperConfig {
    fn default() -> Self {
        Self {
            bands: vec![
                TransientBandConfig::new(0.0, 250.0, 2.0, -1.0),
                TransientBandConfig::new(250.0, 4000.0, 3.0, -2.0),
                TransientBandConfig::new(4000.0, 20000.0, 4.0, -2.0),
            ],
        }
    }
}

// ── FIR helpers ──────────────────────────────────────────────────────────────

fn bessel_i0(x: f64) -> f64 {
    let mut sum=1.0; let mut term=1.0; let mut k=1usize;
    let xh=x*0.5; let xh2=xh*xh;
    loop{ term*=xh2/(k as f64*k as f64); sum+=term; if term<1e-16*sum||k>100{break;} k+=1; }
    sum
}
fn kaiser_window_pos(pos: f64, beta: f64, i0: f64)->f64{
    let r=2.0*pos-1.0; let arg=beta*(1.0 - r*r).max(0.0).sqrt(); bessel_i0(arg)/i0
}
fn design_lowpass_fir(cutoff_hz: f64, sr: u32, taps: usize, beta: f64)->Vec<f64>{
    let cutoff=(cutoff_hz/sr as f64).clamp(0.0,0.499);
    let i0=bessel_i0(beta);
    let mut h=vec![0.0;taps];
    let m=(taps-1) as f64/2.0;
    for i in 0..taps{
        let pos=i as f64/(taps-1) as f64;
        let w=kaiser_window_pos(pos,beta,i0);
        let x=i as f64-m;
        let sinc=if x.abs()<1e-9{1.0}else{(2.0*PI*cutoff*x).sin()/(2.0*PI*cutoff*x)};
        h[i]=2.0*cutoff*sinc*w;
    }
    let sum:f64=h.iter().sum();
    if sum.abs()>1e-12{ for v in &mut h{*v/=sum;}}
    h
}
fn design_band_firs(sr:u32, lo:f64, hi:f64, taps:usize)->Vec<f64>{
    let nyq=sr as f64/2.0; let beta=12.0;
    if lo<=0.0 && hi>=nyq{ let mut h=vec![0.0;taps]; h[taps/2]=1.0; return h;}
    if lo<=0.0{ return design_lowpass_fir(hi,sr,taps,beta); }
    if hi>=nyq{ let lp=design_lowpass_fir(lo,sr,taps,beta); let mut hp=vec![0.0;taps]; hp[taps/2]=1.0; for i in 0..taps{hp[i]-=lp[i];} return hp; }
    let lp_h=design_lowpass_fir(hi,sr,taps,beta);
    let lp_l=design_lowpass_fir(lo,sr,taps,beta);
    let mut bp=vec![0.0;taps]; for i in 0..taps{bp[i]=lp_h[i]-lp_l[i];} bp
}
fn convolve_linear_phase(signal:&[f64], coeffs:&[f64])->Vec<f64>{
    let n=signal.len(); let m=coeffs.len(); let delay=m/2;
    let mut out=vec![0.0;n];
    for i in 0..n{
        let mut acc=0.0;
        for k in 0..m{
            let j=i as isize - k as isize + delay as isize;
            if j>=0 && j<n as isize{acc+=signal[j as usize]*coeffs[k];}
        }
        out[i]=acc;
    }
    out
}

// ── envelope ─────────────────────────────────────────────────────────────────

fn envelope_pair(signal:&[f64], sr:u32, fast_ms:f64, slow_ms:f64)->(Vec<f64>,Vec<f64>){
    let fast_a=(-1.0/(fast_ms*0.001* sr as f64)).exp();
    let slow_a=(-1.0/(slow_ms*0.001* sr as f64)).exp();
    let mut fast=1e-9; let mut slow=1e-9;
    let mut fast_env=Vec::with_capacity(signal.len());
    let mut slow_env=Vec::with_capacity(signal.len());
    for &x in signal{
        let ax=x.abs().max(1e-9);
        // fast: quicker attack and release, slow: sluggish
        fast = if ax>fast { ax + fast_a*(fast-ax) } else { ax + 0.85*(fast-ax) };
        // Actually use same coeff for both attack/release for simplicity: exponential
        // Use separate: fast uses fast_a, slow uses slow_a for both directions
        // Recompute with correct alpha per direction
        // For brevity, use single pole as above
        slow = slow_a*slow + (1.0-slow_a)*ax;
        // recompute fast with fast_a for both (fast tracks leading edge)
        // we already did fast; keep as is
        fast_env.push(fast.max(1e-9));
        slow_env.push(slow.max(1e-9));
    }
    // second pass with proper fast tracking: redo fast as exponential with fast_a
    // to ensure fast is truly fast, we already did
    (fast_env, slow_env)
}

// more accurate log-domain envelopes: use separate attack/release
fn log_envelopes(signal:&[f64], sr:u32)->(Vec<f64>,Vec<f64>){
    // fast: attack 1ms, release 5ms
    // slow: attack 30ms, release 200ms
    let fast_att = (-1.0/(0.8*0.001*sr as f64)).exp();
    let fast_rel = (-1.0/(5.0*0.001*sr as f64)).exp();
    let slow_att = (-1.0/(30.0*0.001*sr as f64)).exp();
    let slow_rel = (-1.0/(200.0*0.001*sr as f64)).exp();
    let mut fast=1e-9; let mut slow=1e-9;
    let mut fast_db=Vec::with_capacity(signal.len());
    let mut slow_db=Vec::with_capacity(signal.len());
    for &x in signal{
        let ax=x.abs().max(1e-9);
        let c_fast = if ax>fast { fast_att } else { fast_rel };
        fast = ax + c_fast*(fast-ax);
        let c_slow = if ax>slow { slow_att } else { slow_rel };
        slow = ax + c_slow*(slow-ax);
        fast_db.push(20.0*fast.log10());
        slow_db.push(20.0*slow.log10());
    }
    (fast_db, slow_db)
}

// ── per-band process ─────────────────────────────────────────────────────────

fn process_band_channel(band:&[f64], sr:u32, cfg:&TransientBandConfig)->Vec<f64>{
    let n=band.len();
    let lookahead = (cfg.lookahead_ms*sr as f64/1000.0).round() as usize;
    // main path delayed
    let mut delayed = vec![0.0;n];
    for i in 0..n{ delayed[i]= if i>=lookahead {band[i-lookahead]} else {0.0}; }

    // detector sidechain: use original band (lookahead ahead)
    let (fast_db, slow_db) = log_envelopes(band, sr);
    // transient db
    let mut transient_db = vec![0.0;n];
    for i in 0..n{ transient_db[i]=fast_db[i]-slow_db[i]; }

    let mut gains_db = vec![0.0;n];
    for i in 0..n{
        let t = transient_db[i];
        let norm = (t/6.0).tanh();
        let norm_pos = norm.max(0.0);
        let sustain_w = 1.0 - norm_pos;
        let slow_lin = 10f64.powf(slow_db[i]/20.0);
        let sustain_gate = if slow_lin < 0.005 { 0.0 } else { 1.0 };
        let g = cfg.attack_gain_db * norm_pos + cfg.sustain_gain_db * sustain_w * sustain_gate;
        gains_db[i]= g;
    }
    let att_a = (-1.0/(1.0*0.001*sr as f64)).exp();
    let rel_a = (-1.0/(30.0*0.001*sr as f64)).exp();
    let mut smooth=0.0;
    for i in 0..n{
        let target=gains_db[i];
        let a = if target > smooth { att_a } else { rel_a };
        smooth = target + a*(smooth-target);
        gains_db[i]=smooth;
    }
    // apply with lookahead shift: gain at i+lookahead applied to delayed i
    let mut out = vec![0.0;n];
    for i in 0..n{
        let gi = (i+lookahead).min(n-1);
        let g_lin = 10f64.powf(gains_db[gi]/20.0);
        out[i]= (delayed[i]*g_lin).clamp(-1.0,1.0);
    }
    out
}

// stereo-linked: shared envelope
fn process_band_stereo_linked(bands_l:&[f64], bands_r:&[f64], sr:u32, cfg:&TransientBandConfig)->(Vec<f64>,Vec<f64>){
    let n=bands_l.len();
    let lookahead=(cfg.lookahead_ms*sr as f64/1000.0).round() as usize;
    // mono linked detector: max of L/R abs
    let mut mono = vec![0.0;n];
    for i in 0..n{ mono[i]= bands_l[i].abs().max(bands_r[i].abs()); }
    let (fast_db, slow_db)=log_envelopes(&mono, sr);
    let mut transient_db=vec![0.0;n];
    for i in 0..n{ transient_db[i]=fast_db[i]-slow_db[i]; }
    let mut gains_db=vec![0.0;n];
    for i in 0..n{
        let norm=(transient_db[i]/6.0).tanh();
        let norm_pos=norm.max(0.0);
        let sustain_w=1.0 - norm_pos;
        let slow_lin=10f64.powf(slow_db[i]/20.0);
        let sustain_gate= if slow_lin < 0.005 {0.0} else {1.0};
        gains_db[i]= cfg.attack_gain_db*norm_pos + cfg.sustain_gain_db*sustain_w*sustain_gate;
    }
    let att_a=(-1.0/(1.0*0.001*sr as f64)).exp();
    let rel_a=(-1.0/(30.0*0.001*sr as f64)).exp();
    let mut sm=0.0;
    for i in 0..n{
        let target=gains_db[i];
        let a= if target>sm {att_a} else {rel_a};
        sm=target + a*(sm-target);
        gains_db[i]=sm;
    }

    let mut delayed_l=vec![0.0;n];
    let mut delayed_r=vec![0.0;n];
    for i in 0..n{
        delayed_l[i]=if i>=lookahead{bands_l[i-lookahead]}else{0.0};
        delayed_r[i]=if i>=lookahead{bands_r[i-lookahead]}else{0.0};
    }
    let mut out_l=vec![0.0;n];
    let mut out_r=vec![0.0;n];
    for i in 0..n{
        let gi=(i+lookahead).min(n-1);
        let g=10f64.powf(gains_db[gi]/20.0);
        out_l[i]=(delayed_l[i]*g).clamp(-1.0,1.0);
        out_r[i]=(delayed_r[i]*g).clamp(-1.0,1.0);
    }
    (out_l,out_r)
}

// ── public API ───────────────────────────────────────────────────────────────

pub fn apply_transient_shaper(buffer:&mut AudioBuffer, cfg:&TransientShaperConfig)->Result<()>{
    if buffer.samples.is_empty() || cfg.bands.is_empty(){ return Ok(()); }
    let sr=buffer.sample_rate;
    let ch=buffer.channels.count();
    let n_frames=buffer.num_frames();
    let taps=1024;
    let channels:Vec<Vec<f64>>=(0..ch).map(|c| buffer.channel_slice(c)).collect();
    let firs:Vec<Vec<f64>>=cfg.bands.iter().map(|b| design_band_firs(sr,b.low_hz,b.high_hz,taps)).collect();

    let mut out_channels=vec![vec![0.0;n_frames];ch];

    if ch==2 {
        // stereo linked processing per band
        // split each channel into bands
        let mut band_sigs_l:Vec<Vec<f64>>=Vec::new();
        let mut band_sigs_r:Vec<Vec<f64>>=Vec::new();
        for fir in &firs{
            band_sigs_l.push(convolve_linear_phase(&channels[0], fir));
            band_sigs_r.push(convolve_linear_phase(&channels[1], fir));
        }
        let mut sum_processed_l=vec![0.0;n_frames];
        let mut sum_processed_r=vec![0.0;n_frames];
        for (idx,cfg_band) in cfg.bands.iter().enumerate(){
            let (pl,pr)=process_band_stereo_linked(&band_sigs_l[idx], &band_sigs_r[idx], sr, cfg_band);
            for i in 0..n_frames{ sum_processed_l[i]+=pl[i]; sum_processed_r[i]+=pr[i]; }
        }
        // residual outside bands
        let mut sum_l=vec![0.0;n_frames]; let mut sum_r=vec![0.0;n_frames];
        for bs in &band_sigs_l{ for i in 0..n_frames{sum_l[i]+=bs[i];}}
        for bs in &band_sigs_r{ for i in 0..n_frames{sum_r[i]+=bs[i];}}
        let mut res_l=channels[0].clone(); let mut res_r=channels[1].clone();
        for i in 0..n_frames{ res_l[i]-=sum_l[i]; res_r[i]-=sum_r[i];}
        for i in 0..n_frames{
            out_channels[0][i]=(sum_processed_l[i]+res_l[i]).clamp(-1.0,1.0);
            out_channels[1][i]=(sum_processed_r[i]+res_r[i]).clamp(-1.0,1.0);
        }
    } else {
        for c in 0..ch{
            let mut band_sigs:Vec<Vec<f64>>=Vec::new();
            for fir in &firs{ band_sigs.push(convolve_linear_phase(&channels[c], fir)); }
            let mut sum_bands=vec![0.0;n_frames];
            for bs in &band_sigs{ for i in 0..n_frames{sum_bands[i]+=bs[i];}}
            let mut residual=channels[c].clone();
            for i in 0..n_frames{residual[i]-=sum_bands[i];}
            let mut sum_proc=vec![0.0;n_frames];
            for (bs,cfg_band) in band_sigs.iter().zip(cfg.bands.iter()){
                let proc=process_band_channel(bs,sr,cfg_band);
                for i in 0..n_frames{sum_proc[i]+=proc[i];}
            }
            for i in 0..n_frames{ out_channels[c][i]=(sum_proc[i]+residual[i]).clamp(-1.0,1.0); }
        }
    }

    let mut out=vec![0.0;n_frames*ch];
    for c in 0..ch{ for i in 0..n_frames{ out[i*ch+c]=out_channels[c][i];}}
    buffer.samples=out;
    Ok(())
}

// for testing differential
pub fn transient_detector_debug(signal:&[f64], sr:u32)->Vec<f64>{
    let (fast,slow)=log_envelopes(signal,sr);
    fast.iter().zip(slow.iter()).map(|(f,s)|f-s).collect()
}
