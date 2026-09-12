use thiserror::Error;

#[derive(Debug, Error)]
pub enum MasteringError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("WAV error: {0}")]
    Wav(#[from] hound::Error),

    #[error("FLAC decode error: {0}")]
    FlacDecode(#[from] claxon::Error),

    #[error("FLAC encode error: {0}")]
    FlacEncode(String),

    #[error("MP3 decode error: {0}")]
    Mp3Decode(String),

    #[error("MP3 encode error: {0}")]
    Mp3Encode(String),

    #[error("Unsupported format: {0}")]
    UnsupportedFormat(String),

    #[error("Invalid EQ filter: {0}")]
    InvalidEqFilter(String),

    #[error("Invalid parameter: {0}")]
    InvalidParameter(String),
}

pub type Result<T> = std::result::Result<T, MasteringError>;
