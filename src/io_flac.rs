use std::fs;
use std::path::Path;
use claxon::FlacReader;
use flac_io::{FlacAudio, encode as flac_encode};
use crate::audio::{AudioBuffer, BitDepth, Channels};
use crate::dsp;
use crate::error::{MasteringError, Result};

pub fn read_flac<P: AsRef<Path>>(path: P) -> Result<AudioBuffer> {
    let mut reader = FlacReader::open(path)?;
    let info = reader.streaminfo();

    let channels = if info.channels == 1 { Channels::Mono } else { Channels::Stereo };
    let bits = info.bits_per_sample;
    let max_val = (1i64 << (bits - 1)) as f64;

    let mut samples = Vec::new();
    let mut blocks = reader.blocks();

    loop {
        match blocks.read_next_or_eof(Vec::new()) {
            Ok(Some(block)) => {
                let num_ch = block.channels() as usize;
                let num_s  = block.duration() as usize;
                for i in 0..num_s {
                    for ch in 0..num_ch {
                        samples.push(block.sample(ch as u32, i as u32) as f64 / max_val);
                    }
                }
            }
            Ok(None) => break,
            Err(e) => return Err(MasteringError::FlacDecode(e)),
        }
    }

    Ok(AudioBuffer::new(samples, info.sample_rate, channels))
}

pub fn write_flac<P: AsRef<Path>>(path: P, buffer: &AudioBuffer, depth: BitDepth) -> Result<()> {
    let bits = depth.as_u16() as u8;
    let num_ch = buffer.channels.count();
    let num_frames = buffer.num_frames();

    let mut ch_samples: Vec<Vec<i32>> = (0..num_ch)
        .map(|_| Vec::with_capacity(num_frames))
        .collect();

    for frame in 0..num_frames {
        for ch in 0..num_ch {
            let s = buffer.samples[frame * num_ch + ch];
            let quantized = quantize(s, bits);
            ch_samples[ch].push(quantized);
        }
    }

    let audio = FlacAudio {
        sample_rate: buffer.sample_rate,
        channels: num_ch as u8,
        bits_per_sample: bits,
        samples: ch_samples,
    };

    let bytes = flac_encode(&audio)
        .map_err(|e| MasteringError::FlacEncode(format!("{:?}", e)))?;

    fs::write(path, bytes)?;
    Ok(())
}

fn quantize(s: f64, bits: u8) -> i32 {
    match bits {
        16 => dsp::f64_to_i16(s) as i32,
        24 => dsp::f64_to_i24(s),
        32 => dsp::f64_to_i32(s),
        _  => dsp::f64_to_i24(s),
    }
}
