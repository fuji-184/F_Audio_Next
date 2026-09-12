use crate::error::{MasteringError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channels {
    Mono,
    Stereo,
}

impl Channels {
    pub fn count(self) -> usize {
        match self {
            Channels::Mono => 1,
            Channels::Stereo => 2,
        }
    }

    pub fn from_str(s: &str) -> Result<Self> {
        match s.to_lowercase().as_str() {
            "mono" => Ok(Channels::Mono),
            "stereo" => Ok(Channels::Stereo),
            other => Err(MasteringError::InvalidParameter(format!(
                "unknown channel mode '{}', expected 'mono' or 'stereo'",
                other
            ))),
        }
    }
}

/// Bit depth for WAV and FLAC output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BitDepth {
    Bits16,
    Bits24,
    Bits32,
}

impl BitDepth {
    pub fn from_u16(bits: u16) -> Result<Self> {
        match bits {
            16 => Ok(BitDepth::Bits16),
            24 => Ok(BitDepth::Bits24),
            32 => Ok(BitDepth::Bits32),
            other => Err(MasteringError::InvalidParameter(format!(
                "unsupported bit depth {}, expected 16, 24, or 32",
                other
            ))),
        }
    }

    pub fn as_u16(self) -> u16 {
        match self {
            BitDepth::Bits16 => 16,
            BitDepth::Bits24 => 24,
            BitDepth::Bits32 => 32,
        }
    }
}

/// Interleaved PCM samples, f64 normalized to [-1.0, 1.0].
#[derive(Debug, Clone)]
pub struct AudioBuffer {
    pub samples: Vec<f64>,
    pub sample_rate: u32,
    pub channels: Channels,
}

impl AudioBuffer {
    pub fn new(samples: Vec<f64>, sample_rate: u32, channels: Channels) -> Self {
        Self { samples, sample_rate, channels }
    }

    pub fn from_mono(samples: Vec<f64>, sample_rate: u32) -> Self {
        Self::new(samples, sample_rate, Channels::Mono)
    }

    pub fn from_channels(left: &[f64], right: &[f64], sample_rate: u32) -> Self {
        let len = left.len().min(right.len());
        let mut interleaved = Vec::with_capacity(len * 2);
        for i in 0..len {
            interleaved.push(left[i]);
            interleaved.push(right[i]);
        }
        Self::new(interleaved, sample_rate, Channels::Stereo)
    }

    pub fn num_frames(&self) -> usize {
        self.samples.len() / self.channels.count()
    }

    pub fn channel_slice(&self, ch: usize) -> Vec<f64> {
        let n = self.channels.count();
        self.samples.iter().skip(ch).step_by(n).copied().collect()
    }
}
