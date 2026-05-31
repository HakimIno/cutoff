//! Audio decoding, waveform extraction, and cpal-driven output.

pub mod decoder;
pub mod output;
pub mod waveform;

pub use decoder::{AudioFrame, FfmpegAudioDecoder};
pub use waveform::{extract_waveform, load_waveform, WaveformPeaks};

/// Canonical preview audio format. All decoders resample to this so the
/// cpal output stream config is fixed and predictable.
pub const SAMPLE_RATE: u32 = 48_000;
pub const CHANNELS: u16 = 2;
