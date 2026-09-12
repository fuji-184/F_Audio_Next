use std::f64::consts::PI;
use crate::error::{MasteringError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterType {
    LowCut,
    LowCut1,
    LowCutGain,
    LowShelf,
    LowShelf2,
    DigitalBell,
    DigitalBell2,
    AnalogBell,
    HighShelf,
    HighShelf2,
    HighCut,
    HighCut1,
    HighCutGain,
    BandpassGain,
    NotchGain,
}

impl FilterType {
    pub fn from_str(name: &str) -> Result<Self> {
        match name.to_lowercase().replace(['-', '_'], " ").as_str() {
            "low cut" | "lowcut"                                   => Ok(Self::LowCut),
            "low cut 1" | "lowcut1" | "low cut1"                  => Ok(Self::LowCut1),
            "low cut gain" | "lowcut gain" | "lowcutgain"
            | "low cut+gain"                                       => Ok(Self::LowCutGain),
            "low shelf" | "lowshelf"                               => Ok(Self::LowShelf),
            "low shelf 2" | "lowshelf2" | "low shelf2"            => Ok(Self::LowShelf2),
            "digital bell" | "digitalbell"                         => Ok(Self::DigitalBell),
            "digital bell 2" | "digitalbell2" | "digital bell2"   => Ok(Self::DigitalBell2),
            "analog bell" | "analogbell"                           => Ok(Self::AnalogBell),
            "high shelf" | "highshelf"                             => Ok(Self::HighShelf),
            "high shelf 2" | "highshelf2" | "high shelf2"         => Ok(Self::HighShelf2),
            "high cut" | "highcut"                                 => Ok(Self::HighCut),
            "high cut 1" | "highcut1" | "high cut1"               => Ok(Self::HighCut1),
            "high cut gain" | "highcut gain" | "highcutgain"
            | "high cut+gain"                                      => Ok(Self::HighCutGain),
            "bandpass" | "bandpass gain" | "bandpassgain"
            | "bandpass+gain"                                      => Ok(Self::BandpassGain),
            "notch" | "notch gain" | "notchgain" | "notch+gain"   => Ok(Self::NotchGain),
            other => Err(MasteringError::InvalidEqFilter(format!("unknown filter '{}'", other))),
        }
    }
}

// ── biquad ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
pub struct BiquadCoeffs {
    pub b0: f64,
    pub b1: f64,
    pub b2: f64,
    pub a1: f64,
    pub a2: f64,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct BiquadState {
    x1: f64,
    x2: f64,
    y1: f64,
    y2: f64,
}

impl BiquadState {
    pub fn process(&mut self, x: f64, c: &BiquadCoeffs) -> f64 {
        let y = c.b0 * x + c.b1 * self.x1 + c.b2 * self.x2
              - c.a1 * self.y1 - c.a2 * self.y2;
        self.x2 = self.x1; self.x1 = x;
        self.y2 = self.y1; self.y1 = y;
        y
    }
}

// ── EQ band ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct EqBand {
    pub filter_type: FilterType,
    pub frequency: f64,
    pub gain_db: f64,
    pub q: f64,
}

impl EqBand {
    pub fn new(name: &str, frequency: f64, gain_db: f64, q: f64) -> Result<Self> {
        if frequency <= 0.0 {
            return Err(MasteringError::InvalidParameter("frequency must be > 0".into()));
        }
        if q <= 0.0 {
            return Err(MasteringError::InvalidParameter("Q must be > 0".into()));
        }
        Ok(Self { filter_type: FilterType::from_str(name)?, frequency, gain_db, q })
    }

    pub fn to_biquad(&self, sample_rate: u32) -> BiquadCoeffs {
        compute_biquad(self.filter_type, self.frequency, self.gain_db, self.q, sample_rate as f64)
    }
}

// ── coefficient computation ───────────────────────────────────────────────────

fn compute_biquad(filter: FilterType, freq: f64, gain_db: f64, q: f64, fs: f64) -> BiquadCoeffs {
    let w0      = 2.0 * PI * freq / fs;
    let cos_w0  = w0.cos();
    let sin_w0  = w0.sin();
    let alpha   = sin_w0 / (2.0 * q);
    let a       = 10f64.powf(gain_db / 40.0); // amplitude (sqrt of power gain)

    match filter {
        FilterType::DigitalBell | FilterType::DigitalBell2 => bell_digital(cos_w0, alpha, a),
        FilterType::AnalogBell                             => bell_analog(cos_w0, sin_w0, alpha, a, q),
        FilterType::LowShelf  | FilterType::LowShelf2     => low_shelf(cos_w0, sin_w0, a, q),
        FilterType::HighShelf | FilterType::HighShelf2     => high_shelf(cos_w0, sin_w0, a, q),
        FilterType::LowCut                                 => low_cut_2nd(cos_w0, alpha),
        FilterType::HighCut                                => high_cut_2nd(cos_w0, alpha),
        FilterType::LowCut1                                => low_cut_1st(w0),
        FilterType::HighCut1                               => high_cut_1st(w0),
        FilterType::LowCutGain                             => low_cut_gain(cos_w0, alpha, a),
        FilterType::HighCutGain                            => high_cut_gain(cos_w0, alpha, a),
        FilterType::BandpassGain                           => bandpass_gain(cos_w0, alpha, a),
        FilterType::NotchGain                              => notch_gain(cos_w0, alpha, a),
    }
}

fn norm(b0: f64, b1: f64, b2: f64, a0: f64, a1: f64, a2: f64) -> BiquadCoeffs {
    BiquadCoeffs { b0: b0/a0, b1: b1/a0, b2: b2/a0, a1: a1/a0, a2: a2/a0 }
}

fn bell_digital(cos_w0: f64, alpha: f64, a: f64) -> BiquadCoeffs {
    norm(
        1.0 + alpha * a,  -2.0 * cos_w0,  1.0 - alpha * a,
        1.0 + alpha / a,  -2.0 * cos_w0,  1.0 - alpha / a,
    )
}

fn bell_analog(cos_w0: f64, sin_w0: f64, _alpha: f64, a: f64, q: f64) -> BiquadCoeffs {
    let bw = sin_w0 / q;
    norm(
        1.0 + bw * a,  -2.0 * cos_w0,  1.0 - bw * a,
        1.0 + bw / a,  -2.0 * cos_w0,  1.0 - bw / a,
    )
}

fn low_shelf(cos_w0: f64, sin_w0: f64, a: f64, q: f64) -> BiquadCoeffs {
    let s    = 1.0 / q;
    let beta = sin_w0 * ((a + 1.0/a) * (1.0/s - 1.0) + 2.0).sqrt();
    norm(
         a * ((a+1.0) - (a-1.0)*cos_w0 + beta),
         2.0 * a * ((a-1.0) - (a+1.0)*cos_w0),
         a * ((a+1.0) - (a-1.0)*cos_w0 - beta),
         (a+1.0) + (a-1.0)*cos_w0 + beta,
        -2.0 * ((a-1.0) + (a+1.0)*cos_w0),
         (a+1.0) + (a-1.0)*cos_w0 - beta,
    )
}

fn high_shelf(cos_w0: f64, sin_w0: f64, a: f64, q: f64) -> BiquadCoeffs {
    let s    = 1.0 / q;
    let beta = sin_w0 * ((a + 1.0/a) * (1.0/s - 1.0) + 2.0).sqrt();
    norm(
         a * ((a+1.0) + (a-1.0)*cos_w0 + beta),
        -2.0 * a * ((a-1.0) + (a+1.0)*cos_w0),
         a * ((a+1.0) + (a-1.0)*cos_w0 - beta),
         (a+1.0) - (a-1.0)*cos_w0 + beta,
         2.0 * ((a-1.0) - (a+1.0)*cos_w0),
         (a+1.0) - (a-1.0)*cos_w0 - beta,
    )
}

fn low_cut_2nd(cos_w0: f64, alpha: f64) -> BiquadCoeffs {
    norm(
        (1.0 + cos_w0) / 2.0,  -(1.0 + cos_w0),  (1.0 + cos_w0) / 2.0,
         1.0 + alpha,           -2.0 * cos_w0,     1.0 - alpha,
    )
}

fn high_cut_2nd(cos_w0: f64, alpha: f64) -> BiquadCoeffs {
    norm(
        (1.0 - cos_w0) / 2.0,  1.0 - cos_w0,  (1.0 - cos_w0) / 2.0,
         1.0 + alpha,          -2.0 * cos_w0,   1.0 - alpha,
    )
}

fn low_cut_1st(w0: f64) -> BiquadCoeffs {
    let k = (w0 / 2.0).tan();
    let n = 1.0 + k;
    BiquadCoeffs { b0: 1.0/n, b1: -1.0/n, b2: 0.0, a1: (k-1.0)/n, a2: 0.0 }
}

fn high_cut_1st(w0: f64) -> BiquadCoeffs {
    let k = (w0 / 2.0).tan();
    let n = 1.0 + k;
    BiquadCoeffs { b0: k/n, b1: k/n, b2: 0.0, a1: (k-1.0)/n, a2: 0.0 }
}

fn low_cut_gain(cos_w0: f64, alpha: f64, a: f64) -> BiquadCoeffs {
    norm(
        a * (1.0-cos_w0)/2.0,  a * (1.0-cos_w0),  a * (1.0-cos_w0)/2.0,
        1.0 + alpha,           -2.0 * cos_w0,       1.0 - alpha,
    )
}

fn high_cut_gain(cos_w0: f64, alpha: f64, a: f64) -> BiquadCoeffs {
    norm(
        a * (1.0+cos_w0)/2.0,  -a * (1.0+cos_w0),  a * (1.0+cos_w0)/2.0,
        1.0 + alpha,            -2.0 * cos_w0,       1.0 - alpha,
    )
}

fn bandpass_gain(cos_w0: f64, alpha: f64, a: f64) -> BiquadCoeffs {
    norm(
        alpha * a,  0.0,  -alpha * a,
        1.0 + alpha, -2.0 * cos_w0, 1.0 - alpha,
    )
}

fn notch_gain(cos_w0: f64, alpha: f64, a: f64) -> BiquadCoeffs {
    norm(
        a * (1.0 + alpha),  -2.0 * a * cos_w0,  a * (1.0 - alpha),
        1.0 + alpha,        -2.0 * cos_w0,       1.0 - alpha,
    )
}

// ── apply to buffer ───────────────────────────────────────────────────────────

pub fn apply_eq_band(samples: &mut [f64], band: &EqBand, sample_rate: u32, num_channels: usize) {
    let coeffs = band.to_biquad(sample_rate);
    let mut states = vec![BiquadState::default(); num_channels];
    for frame in samples.chunks_mut(num_channels) {
        for (ch, sample) in frame.iter_mut().enumerate() {
            *sample = states[ch].process(*sample, &coeffs);
        }
    }
}
