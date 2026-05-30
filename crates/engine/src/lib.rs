//! Infrastructure layer: external media engines (FFmpeg, GStreamer).
//!
//! The [`MergeEngine`] trait is the only abstraction the rest of the app
//! depends on; pick a concrete implementation at composition time.

pub mod decoder;
pub mod ffmpeg;
pub mod traits;

pub use decoder::{DecodedFrame, DecoderError, DecoderResult, FfmpegMediaDecoder, MediaDecoder};

// Future backend; behind a feature flag once GStreamer system libs are needed.
// pub mod gstreamer;

use thiserror::Error;

pub use traits::{MergeEngine, ProgressEvent, ProgressSink};

#[derive(Debug, Error)]
pub enum EngineError {
    #[error("media engine binary not found on PATH: {0}")]
    BinaryMissing(String),

    #[error("media engine exited with status {0}")]
    NonZeroExit(i32),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("probe parse error: {0}")]
    ProbeParse(String),

    #[error("job was cancelled")]
    Cancelled,
}

pub type EngineResult<T> = Result<T, EngineError>;
