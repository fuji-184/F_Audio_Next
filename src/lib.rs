pub mod audio;
pub mod compressor;
pub mod de_esser;
pub mod dsp;
pub mod eq;
pub mod error;
pub mod harmonic_exciter;
pub mod io;
pub mod io_flac;
pub mod io_mp3;
pub mod io_wav;
pub mod limiter;
pub mod normalization;
pub mod noise_reduction;
pub mod track;
pub mod transient_shaper;

pub use audio::{AudioBuffer, BitDepth, Channels};
pub use error::{MasteringError, Result};
pub use track::Track;

/// Read an audio file (WAV, FLAC, or MP3). Panics on error.
///
/// # Example
/// ```no_run
/// let mut input = f_audio_mastering::read("./music.wav");
///
/// input.eq("digital bell 2", 100.0, 2.0, 3.0)
///      .eq("high cut", 16000.0, 0.0, 1.4);
///
/// input.save_wav("./output", 48000, "stereo", 24).unwrap();
/// input.save_flac("./output", 48000, "stereo", 32).unwrap();
/// input.save_mp3("./output", 320, "stereo").unwrap();
/// ```
pub fn read<P: AsRef<std::path::Path>>(path: P) -> Track {
    let buffer = io::read(path).unwrap_or_else(|e| panic!("Failed to read audio file: {}", e));
    Track::new(buffer)
}

/// Read an audio file, returning a [`Result`] instead of panicking.
pub fn try_read<P: AsRef<std::path::Path>>(path: P) -> Result<Track> {
    Ok(Track::new(io::read(path)?))
}
