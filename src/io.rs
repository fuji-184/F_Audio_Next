use std::path::Path;
use crate::audio::{AudioBuffer, BitDepth};
use crate::error::{MasteringError, Result};
use crate::io_flac;
use crate::io_mp3;
use crate::io_wav;

enum AudioFormat { Wav, Flac, Mp3 }

fn detect_format<P: AsRef<Path>>(path: P) -> Result<AudioFormat> {
    let ext = path
        .as_ref()
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    match ext.as_str() {
        "wav" | "wave" => Ok(AudioFormat::Wav),
        "flac"         => Ok(AudioFormat::Flac),
        "mp3"          => Ok(AudioFormat::Mp3),
        other => Err(MasteringError::UnsupportedFormat(format!(
            "unknown extension '{}', supported: wav, flac, mp3",
            other
        ))),
    }
}

pub fn read<P: AsRef<Path>>(path: P) -> Result<AudioBuffer> {
    match detect_format(&path)? {
        AudioFormat::Wav  => io_wav::read_wav(path),
        AudioFormat::Flac => io_flac::read_flac(path),
        AudioFormat::Mp3  => io_mp3::read_mp3(path),
    }
}

pub fn write_wav<P: AsRef<Path>>(path: P, buffer: &AudioBuffer, depth: BitDepth) -> Result<()> {
    io_wav::write_wav(path, buffer, depth)
}

pub fn write_flac<P: AsRef<Path>>(path: P, buffer: &AudioBuffer, depth: BitDepth) -> Result<()> {
    io_flac::write_flac(path, buffer, depth)
}

pub fn write_mp3<P: AsRef<Path>>(path: P, buffer: &AudioBuffer, kbps: u32) -> Result<()> {
    io_mp3::write_mp3(path, buffer, kbps)
}

pub fn ensure_ext(path: &str, ext: &str) -> String {
    if path.ends_with(&format!(".{}", ext)) {
        path.to_string()
    } else {
        format!("{}.{}", path, ext)
    }
}
