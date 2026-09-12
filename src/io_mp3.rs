use std::fs;
use std::path::Path;
use minimp3::{Decoder, Error as Mp3Error, Frame};
use mp3lame_encoder::{Builder, DualPcm, FlushNoGap, MonoPcm};
use crate::audio::{AudioBuffer, Channels};
use crate::dsp;
use crate::error::{MasteringError, Result};

pub fn read_mp3<P: AsRef<Path>>(path: P) -> Result<AudioBuffer> {
    let data = fs::read(path)?;
    let mut decoder = Decoder::new(data.as_slice());

    let mut all_samples: Vec<f64> = Vec::new();
    let mut sample_rate = 44100u32;
    let mut channels = Channels::Stereo;

    loop {
        match decoder.next_frame() {
            Ok(Frame { data, sample_rate: sr, channels: ch, .. }) => {
                sample_rate = sr as u32;
                channels = if ch == 1 { Channels::Mono } else { Channels::Stereo };
                for s in data {
                    all_samples.push(dsp::i16_to_f64(s));
                }
            }
            Err(Mp3Error::Eof) => break,
            Err(e) => return Err(MasteringError::Mp3Decode(format!("{:?}", e))),
        }
    }

    Ok(AudioBuffer::new(all_samples, sample_rate, channels))
}

pub fn write_mp3<P: AsRef<Path>>(path: P, buffer: &AudioBuffer, bitrate_kbps: u32) -> Result<()> {
    let i16_samples: Vec<i16> = buffer.samples.iter().map(|&s| dsp::f64_to_i16(s)).collect();
    let mp3_data = match buffer.channels {
        Channels::Mono  => encode_mono(&i16_samples, buffer.sample_rate, bitrate_kbps)?,
        Channels::Stereo => encode_stereo(&i16_samples, buffer.sample_rate, bitrate_kbps)?,
    };
    fs::write(path, &mp3_data)?;
    Ok(())
}

fn build_encoder(sample_rate: u32, num_channels: u8, bitrate_kbps: u32) -> Result<mp3lame_encoder::Encoder> {
    let mut builder = Builder::new()
        .ok_or_else(|| MasteringError::Mp3Encode("failed to create MP3 encoder".into()))?;
    builder
        .set_num_channels(num_channels)
        .map_err(|e| MasteringError::Mp3Encode(format!("{:?}", e)))?;
    builder
        .set_sample_rate(sample_rate)
        .map_err(|e| MasteringError::Mp3Encode(format!("{:?}", e)))?;
    builder
        .set_brate(kbps_to_lame(bitrate_kbps))
        .map_err(|e| MasteringError::Mp3Encode(format!("{:?}", e)))?;
    builder
        .set_quality(mp3lame_encoder::Quality::Best)
        .map_err(|e| MasteringError::Mp3Encode(format!("{:?}", e)))?;
    builder
        .build()
        .map_err(|e| MasteringError::Mp3Encode(format!("{:?}", e)))
}

fn encode_mono(samples: &[i16], sample_rate: u32, bitrate_kbps: u32) -> Result<Vec<u8>> {
    let mut encoder = build_encoder(sample_rate, 1, bitrate_kbps)?;
    let input = MonoPcm(samples);

    let mut out = Vec::new();
    out.reserve(mp3lame_encoder::max_required_buffer_size(samples.len()));

    let n = encoder
        .encode(input, out.spare_capacity_mut())
        .map_err(|e| MasteringError::Mp3Encode(format!("{:?}", e)))?;
    unsafe { out.set_len(out.len() + n) };

    let n = encoder
        .flush::<FlushNoGap>(out.spare_capacity_mut())
        .map_err(|e| MasteringError::Mp3Encode(format!("{:?}", e)))?;
    unsafe { out.set_len(out.len() + n) };

    Ok(out)
}

fn encode_stereo(interleaved: &[i16], sample_rate: u32, bitrate_kbps: u32) -> Result<Vec<u8>> {
    let mut encoder = build_encoder(sample_rate, 2, bitrate_kbps)?;

    // mp3lame-encoder::DualPcm butuh left dan right terpisah
    let frames = interleaved.len() / 2;
    let mut left  = Vec::with_capacity(frames);
    let mut right = Vec::with_capacity(frames);
    for chunk in interleaved.chunks_exact(2) {
        left.push(chunk[0]);
        right.push(chunk[1]);
    }

    let input = DualPcm { left: &left, right: &right };

    let mut out = Vec::new();
    out.reserve(mp3lame_encoder::max_required_buffer_size(frames));

    let n = encoder
        .encode(input, out.spare_capacity_mut())
        .map_err(|e| MasteringError::Mp3Encode(format!("{:?}", e)))?;
    unsafe { out.set_len(out.len() + n) };

    let n = encoder
        .flush::<FlushNoGap>(out.spare_capacity_mut())
        .map_err(|e| MasteringError::Mp3Encode(format!("{:?}", e)))?;
    unsafe { out.set_len(out.len() + n) };

    Ok(out)
}

fn kbps_to_lame(kbps: u32) -> mp3lame_encoder::Bitrate {
    use mp3lame_encoder::Bitrate::*;
    match kbps {
        8   => Kbps8,   16  => Kbps16,  24  => Kbps24,  32  => Kbps32,
        40  => Kbps40,  48  => Kbps48,  64  => Kbps64,
        80  => Kbps80,  96  => Kbps96,  112 => Kbps112, 128 => Kbps128,
        160 => Kbps160, 192 => Kbps192, 224 => Kbps224, 256 => Kbps256,
        320 => Kbps320,
        _   => Kbps192,
    }
}
