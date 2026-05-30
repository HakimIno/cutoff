//! Real-time media decoding for preview playback and thumbnail extraction.
//!
//! Wraps `ffmpeg-next` (libav* bindings). All FFI-touching state is owned
//! by a single owning thread per session; the public surface is a synchronous
//! pull API (`next_frame`, `seek_to`) that the worker drives from a
//! dedicated tokio task.

pub mod ffmpeg_decoder;
pub mod frame;
pub mod thumbnails;

pub use ffmpeg_decoder::FfmpegMediaDecoder;
pub use frame::DecodedFrame;
pub use thumbnails::extract_thumbnails;

use std::path::Path;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum DecoderError {
    #[error("ffmpeg error: {0}")]
    Ffmpeg(#[from] ffmpeg_next::Error),
    #[error("no video stream in {0}")]
    NoVideoStream(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("image encode error: {0}")]
    ImageEncode(String),
}

pub type DecoderResult<T> = Result<T, DecoderError>;

#[derive(Debug, Clone, Copy)]
pub struct StreamMeta {
    /// Total duration in microseconds (best-effort; some containers under-report).
    pub duration_us: i64,
    /// Frame rate as rational num/den (e.g. 30000/1001).
    pub frame_rate_num: i32,
    pub frame_rate_den: i32,
}

impl StreamMeta {
    pub fn frame_rate_f32(&self) -> f32 {
        if self.frame_rate_den == 0 {
            0.0
        } else {
            self.frame_rate_num as f32 / self.frame_rate_den as f32
        }
    }
}

/// Pull-based decoder session. Not Send (libav state); driven by one task.
pub trait MediaDecoder {
    fn meta(&self) -> StreamMeta;
    fn seek_to_us(&mut self, pts_us: i64) -> DecoderResult<()>;
    fn next_frame(&mut self) -> DecoderResult<Option<DecodedFrame>>;
}

pub fn open(path: &Path) -> DecoderResult<FfmpegMediaDecoder> {
    FfmpegMediaDecoder::open(path)
}
