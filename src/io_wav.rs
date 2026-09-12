use std::path::Path;
use hound::{SampleFormat, WavReader, WavSpec, WavWriter};
use crate::audio::{AudioBuffer, BitDepth, Channels};
use crate::dsp;
use crate::error::{MasteringError, Result};

pub fn read_wav<P: AsRef<Path>>(path: P) -> Result<AudioBuffer> {
    let mut reader = WavReader::open(path)?;
    let spec = reader.spec();

    let channels = if spec.channels == 1 { Channels::Mono } else { Channels::Stereo };

    let samples: Vec<f64> = match (spec.sample_format, spec.bits_per_sample) {
        (SampleFormat::Int, 16) => reader
            .samples::<i16>()
            .map(|s| Ok(dsp::i16_to_f64(s?)))
            .collect::<std::result::Result<_, hound::Error>>()?,
        (SampleFormat::Int, 24) => reader
            .samples::<i32>()
            .map(|s| Ok(dsp::i24_to_f64(s?)))
            .collect::<std::result::Result<_, hound::Error>>()?,
        (SampleFormat::Int, 32) => reader
            .samples::<i32>()
            .map(|s| Ok(dsp::i32_to_f64(s?)))
            .collect::<std::result::Result<_, hound::Error>>()?,
        // Juga handle float 32-bit WAV saat baca (file dari software lain)
        (SampleFormat::Float, 32) => reader
            .samples::<f32>()
            .map(|s| Ok(dsp::f32_to_f64(s?)))
            .collect::<std::result::Result<_, hound::Error>>()?,
        (fmt, bits) => {
            return Err(MasteringError::UnsupportedFormat(format!(
                "WAV {:?} {}bit not supported",
                fmt, bits
            )));
        }
    };

    Ok(AudioBuffer::new(samples, spec.sample_rate, channels))
}

pub fn write_wav<P: AsRef<Path>>(path: P, buffer: &AudioBuffer, depth: BitDepth) -> Result<()> {
    // Semua ditulis sebagai Int agar konsisten dengan reader dan player
    let spec = WavSpec {
        channels: buffer.channels.count() as u16,
        sample_rate: buffer.sample_rate,
        bits_per_sample: depth.as_u16(),
        sample_format: SampleFormat::Int,
    };

    let mut writer = WavWriter::create(path, spec)?;

    match depth {
        BitDepth::Bits16 => {
            for &s in &buffer.samples {
                writer.write_sample(dsp::f64_to_i16(s))?;
            }
        }
        BitDepth::Bits24 => {
            for &s in &buffer.samples {
                writer.write_sample(dsp::f64_to_i24(s))?;
            }
        }
        BitDepth::Bits32 => {
            for &s in &buffer.samples {
                writer.write_sample(dsp::f64_to_i32(s))?;
            }
        }
    }

    writer.finalize()?;
    Ok(())
}
