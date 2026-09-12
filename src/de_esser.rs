use crate::audio::AudioBuffer;
use crate::error::Result;
use std::f64::consts::TAU;

// ── config ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct DeEsserConfig {
    pub low_hz: f64,
    pub high_hz: f64,
    pub threshold_db: f64,
    pub ratio: f64,
    pub knee_db: f64,
    pub lookahead_ms: f64,
    pub stereo_linked: bool,
}

impl Default for DeEsserConfig {
    fn default() -> Self {
        Self {
            low_hz: 4000.0,
            high_hz: 12000.0,
            threshold_db: -18.0,
            ratio: 4.0,
            knee_db: 6.0,
            lookahead_ms: 3.0,
            stereo_linked: true,
        }
    }
}

impl DeEsserConfig {
    pub fn with_threshold(mut self, db: f64) -> Self { self.threshold_db = db; self }
    pub fn with_ratio(mut self, r: f64) -> Self { self.ratio = r.max(1.0); self }
    pub fn with_knee(mut self, k: f64) -> Self { self.knee_db = k; self }
    pub fn with_lookahead(mut self, ms: f64) -> Self { self.lookahead_ms = ms.clamp(0.0, 4.0); self }
}

// ── FFT helpers ──────────────────────────────────────────────────────────────

fn hann_window(n: usize) -> Vec<f64> {
    (0..n).map(|i| 0.5 * (1.0 - (TAU * i as f64 / (n - 1) as f64).cos())).collect()
}
fn fft_inplace(re: &mut [f64], im: &mut [f64], inv: bool) {
    let n = re.len();
    let mut j = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 { j ^= bit; bit >>= 1; }
        j ^= bit;
        if i < j { re.swap(i,j); im.swap(i,j); }
    }
    let sign = if inv { 1.0 } else { -1.0 };
    let mut len = 2;
    while len <= n {
        let half = len/2;
        let ang = sign * TAU / len as f64;
        let (wre,wim) = (ang.cos(), ang.sin());
        for i in (0..n).step_by(len) {
            let (mut ur, mut ui) = (1.0,0.0);
            for k in 0..half {
                let tr = ur*re[i+k+half] - ui*im[i+k+half];
                let ti = ur*im[i+k+half] + ui*re[i+k+half];
                re[i+k+half]=re[i+k]-tr; im[i+k+half]=im[i+k]-ti;
                re[i+k]+=tr; im[i+k]+=ti;
                let nr=ur*wre - ui*wim; let ni=ur*wim + ui*wre; ur=nr; ui=ni;
            }
        }
        len <<=1;
    }
    if inv { let s=1.0/n as f64; for (r,i) in re.iter_mut().zip(im.iter_mut()){*r*=s;*i*=s;}}
}
fn real_fft(input:&[f64])->Vec<(f64,f64)>{
    let n=input.len();
    let mut re=input.to_vec(); let mut im=vec![0.0;n];
    fft_inplace(&mut re,&mut im,false);
    (0..=n/2).map(|i|(re[i],im[i])).collect()
}
fn real_ifft(spec:&[(f64,f64)], n:usize)->Vec<f64>{
    let mut re=vec![0.0;n]; let mut im=vec![0.0;n];
    for (i,&(r,img)) in spec.iter().enumerate(){
        re[i]=r; im[i]=img;
        if i>0 && i<n/2{ re[n-i]=r; im[n-i]=-img;}
    }
    fft_inplace(&mut re,&mut im,true);
    re
}

// ── sibilance detection ──────────────────────────────────────────────────────

fn sibilance_detected(mags:&[f64], freqs:&[f64], low_hz:f64, high_hz:f64) -> (bool, usize, f64, f64) {
    // energy ratio high(4-12k) vs low(<4k) and crest factor in high band
    let mut energy_low: f64 = 1e-12;
    let mut energy_high: f64 = 1e-12;
    let mut peak_high: f64 = 1e-12;
    let mut sum_high = 0.0;
    let mut count_high = 0usize;
    let mut peak_idx = 0usize;
    let mut peak_val: f64 = 0.0;
    for (i,&f) in freqs.iter().enumerate(){
        let m = mags[i];
        let p = m*m;
        if f < low_hz {
            energy_low += p;
        } else if f >= low_hz && f <= high_hz {
            energy_high += p;
            if m > peak_val { peak_val=m; peak_idx=i; }
            peak_high = peak_high.max(m);
            sum_high += m;
            count_high+=1;
        }
    }
    let rms_high = (sum_high / count_high.max(1) as f64).max(1e-12);
    let crest = peak_high / rms_high;
    let crest_db = 20.0*(crest).log10();
    let ratio = energy_high / energy_low;
    // sibilance: high ratio + moderate crest (noise-like but not too peaky)
    // tuned for mastering: ratio >0.3 and crest <14 dB covers both noise and sibilant friction
    let is_sibilance = ratio > 0.3 && crest_db < 14.0 && peak_val > 1e-6;
    let q = if is_sibilance { 1.2 } else { 1.5 };
    (is_sibilance, peak_idx, ratio, q)
}

fn soft_knee_gr(x_db: f64, thresh: f64, ratio: f64, knee: f64) -> f64 {
    let r = ratio.max(1.0);
    if knee < 0.5 {
        if x_db <= thresh {0.0} else {(x_db - thresh)*(1.0-1.0/r)}
    } else {
        let w=knee;
        if x_db < thresh - w/2.0 {0.0}
        else if x_db > thresh + w/2.0 {(x_db - thresh)*(1.0-1.0/r)}
        else { let x=x_db - thresh + w/2.0; x*x*(1.0-1.0/r)/(2.0*w) }
    }
}

// bell shape magnitude in linear gain for linear-phase
fn bell_gain_linear(freq: f64, center: f64, q: f64, gr_db: f64) -> f64 {
    if gr_db.abs() < 1e-6 { return 1.0; }
    let gr_lin = 10f64.powf(-gr_db/20.0); // attenuation <1
    // bell shape: gaussian in log frequency, Q controls width
    // q high => narrow
    let oct = (freq / center).log2().abs();
    let shape = (-0.5 * (oct * q * 1.5).powi(2)).exp();
    // shape 1 at center, 0 far
    1.0 - (1.0 - gr_lin) * shape
}

// ── core processing ──────────────────────────────────────────────────────────

pub fn apply_de_esser(buffer: &mut AudioBuffer, cfg: &DeEsserConfig) -> Result<()> {
    if buffer.samples.is_empty() { return Ok(()); }
    let sr = buffer.sample_rate;
    let ch = buffer.channels.count();
    let n_frames = buffer.num_frames();
    let n = 2048;
    let hop = 512; // 75% overlap
    let hann = hann_window(n);
    let n_bins = n/2+1;
    let freqs: Vec<f64> = (0..n_bins).map(|i| i as f64 * sr as f64 / n as f64).collect();
    let lookahead = (cfg.lookahead_ms * sr as f64 / 1000.0).round() as usize;
    // we implement lookahead by delaying main path and using detector on future
    // For STFT, we will compute gains per frame, then apply with lookahead shift

    let channels: Vec<Vec<f64>> = (0..ch).map(|c| buffer.channel_slice(c)).collect();

    // we need to handle stereo linked: detector uses max of L/R per frame
    // For simplicity, process linked by computing gains from mono-linked mags, then apply same gains to both

    // Prepare per-frame gains and centers
    // Number of frames
    let _num_stft_frames = (n_frames + hop -1)/hop + 2;
    // For each stft frame, compute detection and gain
    // We need to collect gains per bin per frame

    // First, collect stft mags for detector (linked)
    let mut frame_mags_linked: Vec<Vec<f64>> = Vec::new();
    let mut frame_centers: Vec<f64> = Vec::new();
    let mut frame_qs: Vec<f64> = Vec::new();
    let mut frame_gr: Vec<f64> = Vec::new();

    let mut pos = 0usize;
    while pos < n_frames + n {
        // build linked mag for this position (use mono max)
        let mut linked_mag = vec![0.0; n_bins];
        for c in 0..ch {
            let mut frame = vec![0.0; n];
            for i in 0..n {
                let idx = pos + i;
                let s = if idx < n_frames { channels[c][idx] } else {0.0};
                frame[i]= s * hann[i];
            }
            let spec = real_fft(&frame);
            for b in 0..n_bins {
                let m = (spec[b].0*spec[b].0 + spec[b].1*spec[b].1).sqrt();
                if m > linked_mag[b] { linked_mag[b]=m; }
            }
        }
        frame_mags_linked.push(linked_mag);
        pos+=hop;
        if pos > n_frames + n { break; }
    }

    // compute per-frame detection
    for mags in &frame_mags_linked {
        let (is_sib, peak_idx, _ratio, q) = sibilance_detected(mags, &freqs, cfg.low_hz, cfg.high_hz);
        let center = freqs[peak_idx.min(freqs.len()-1)];
        // level in sibilance band for gain computer (RMS of high band)
        let mut sum = 0.0; let mut cnt=0;
        for (i,&f) in freqs.iter().enumerate(){
            if f>=cfg.low_hz && f<=cfg.high_hz { sum+= mags[i]*mags[i]; cnt+=1; }
        }
        let rms = (sum / cnt.max(1) as f64).sqrt().max(1e-9);
        let x_db = 20.0*rms.log10();
        let gr = if is_sib { soft_knee_gr(x_db, cfg.threshold_db, cfg.ratio, cfg.knee_db) } else { 0.0 };
        frame_centers.push(center);
        frame_qs.push(q);
        frame_gr.push(gr);
    }

    // smooth gains over time (attack 2ms, release 30ms)
    let att_a = (-1.0/(2.0*0.001*sr as f64)).exp();
    let rel_a = (-1.0/(30.0*0.001*sr as f64)).exp();
    let mut smooth_gr = 0.0;
    for i in 0..frame_gr.len(){
        let target = frame_gr[i];
        let a = if target > smooth_gr { att_a } else { rel_a };
        smooth_gr = target + a*(smooth_gr - target);
        frame_gr[i]=smooth_gr;
    }

    // Now process each channel with OLA, applying per-frame bell
    let mut out_channels = vec![vec![0.0; n_frames + n]; ch];
    let mut weight = vec![vec![0.0; n_frames + n]; ch];

    let mut frame_idx = 0usize;
    let mut pos2 = 0usize;
    while pos2 < n_frames + n {
        let gr = frame_gr[frame_idx.min(frame_gr.len()-1)];
        let center = frame_centers[frame_idx.min(frame_centers.len()-1)];
        let q = frame_qs[frame_idx.min(frame_qs.len()-1)];
        // lookahead: gain from future frame applied to current main frame
        // main is delayed by lookahead samples => in STFT, this corresponds to shifting frame index
        let lookahead_frames = (lookahead as f64 / hop as f64).round() as usize;
        let gr_la = frame_gr[(frame_idx + lookahead_frames).min(frame_gr.len()-1)];
        let center_la = frame_centers[(frame_idx + lookahead_frames).min(frame_centers.len()-1)];
        let q_la = frame_qs[(frame_idx + lookahead_frames).min(frame_qs.len()-1)];
        let actual_gr = if lookahead>0 { gr_la } else { gr };
        let actual_center = if lookahead>0 { center_la } else { center };
        let actual_q = if lookahead>0 { q_la } else { q };

        for c in 0..ch {
            let mut frame = vec![0.0; n];
            for i in 0..n {
                let idx = pos2 + i;
                let s = if idx < n_frames { channels[c][idx] } else {0.0};
                frame[i]= s * hann[i];
            }
            let mut spec = real_fft(&frame);
            // apply bell attenuation per bin (linear-phase, keep phase)
            for b in 0..n_bins {
                let f = freqs[b];
                if f < 1000.0 { continue; } // only affect highs
                let g_lin = bell_gain_linear(f, actual_center, actual_q, actual_gr);
                spec[b].0 *= g_lin;
                spec[b].1 *= g_lin;
            }
            let time = real_ifft(&spec, n);
            for i in 0..n {
                let idx = pos2 + i;
                if idx < out_channels[c].len(){
                    out_channels[c][idx] += time[i] * hann[i];
                    weight[c][idx] += hann[i]*hann[i];
                }
            }
        }
        pos2+=hop;
        frame_idx+=1;
        if pos2 > n_frames + n { break; }
    }

    let mut out_samples = vec![0.0; n_frames*ch];
    for c in 0..ch {
        for i in 0..n_frames {
            let w = weight[c][i].max(1e-12);
            let v = (out_channels[c][i] / w).clamp(-1.0,1.0);
            out_samples[i*ch + c]=v;
        }
    }
    buffer.samples = out_samples;
    Ok(())
}

// helpers for testing

pub fn sibilance_detection_debug(mags:&[f64], freqs:&[f64], low:f64, high:f64)->(bool,f64,f64){
    let (is, _, ratio, _) = sibilance_detected(mags, freqs, low, high);
    let mut sum: f64 =0.0; let mut cnt=0; let mut peak: f64 =1e-12;
    for (i,&f) in freqs.iter().enumerate(){
        if f>=low && f<=high{ let m=mags[i]; peak=peak.max(m); sum+=m; cnt+=1; }
    }
    let rms = sum/cnt.max(1) as f64;
    let crest = 20.0*(peak/rms.max(1e-12)).log10();
    (is, ratio, crest)
}
