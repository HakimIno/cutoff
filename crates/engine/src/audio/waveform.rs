//! Pre-computed waveform peaks for visualization.
//!
//! For each "bucket" of ~10ms of audio we store one (min, max) pair —
//! enough resolution for the timeline strip without ballooning disk usage
//! (≈ 100 buckets/sec × 8 bytes ≈ 800 bytes per second of audio).

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use super::{decoder::FfmpegAudioDecoder, SAMPLE_RATE};
use crate::decoder::{DecoderError, DecoderResult};

/// Bucket size in audio samples (≈ 10ms at 48kHz).
const BUCKET_SAMPLES: usize = 480;

const MAGIC: &[u8; 4] = b"CWV1";

#[derive(Debug, Clone)]
pub struct WaveformPeaks {
    /// Bucket duration in microseconds. Same for every bucket.
    pub bucket_us: u32,
    /// (min, max) per bucket, downmixed to mono. Range -1.0..=1.0.
    pub peaks: Vec<(f32, f32)>,
}

impl WaveformPeaks {
    pub fn save_to(&self, path: &Path) -> DecoderResult<()> {
        let mut f = std::fs::File::create(path)?;
        f.write_all(MAGIC)?;
        f.write_all(&self.bucket_us.to_le_bytes())?;
        f.write_all(&(self.peaks.len() as u32).to_le_bytes())?;
        for (mn, mx) in &self.peaks {
            f.write_all(&mn.to_le_bytes())?;
            f.write_all(&mx.to_le_bytes())?;
        }
        Ok(())
    }

    pub fn load_from(path: &Path) -> DecoderResult<Self> {
        let mut f = std::fs::File::open(path)?;
        let mut magic = [0u8; 4];
        f.read_exact(&mut magic)?;
        if &magic != MAGIC {
            return Err(DecoderError::ImageEncode("waveform: bad magic".into()));
        }
        let mut buf4 = [0u8; 4];
        f.read_exact(&mut buf4)?;
        let bucket_us = u32::from_le_bytes(buf4);
        f.read_exact(&mut buf4)?;
        let n = u32::from_le_bytes(buf4) as usize;
        let mut peaks = Vec::with_capacity(n);
        for _ in 0..n {
            f.read_exact(&mut buf4)?;
            let mn = f32::from_le_bytes(buf4);
            f.read_exact(&mut buf4)?;
            let mx = f32::from_le_bytes(buf4);
            peaks.push((mn, mx));
        }
        Ok(Self { bucket_us, peaks })
    }
}

/// Decode `source`, downmix to mono, and emit (min, max) per bucket.
pub fn extract_waveform(source: &Path) -> DecoderResult<Option<WaveformPeaks>> {
    let Some(mut decoder) = FfmpegAudioDecoder::open(source)? else {
        return Ok(None);
    };

    let bucket_us =
        (BUCKET_SAMPLES as u64 * 1_000_000 / SAMPLE_RATE as u64) as u32;
    let mut peaks: Vec<(f32, f32)> = Vec::new();
    let mut bucket_min = f32::INFINITY;
    let mut bucket_max = f32::NEG_INFINITY;
    let mut samples_in_bucket = 0usize;

    while let Some(frame) = decoder.next_frame()? {
        // Interleaved stereo → mono average.
        for pair in frame.samples.chunks_exact(2) {
            let mono = 0.5 * (pair[0] + pair[1]);
            if mono < bucket_min {
                bucket_min = mono;
            }
            if mono > bucket_max {
                bucket_max = mono;
            }
            samples_in_bucket += 1;
            if samples_in_bucket >= BUCKET_SAMPLES {
                peaks.push((
                    if bucket_min.is_finite() { bucket_min } else { 0.0 },
                    if bucket_max.is_finite() { bucket_max } else { 0.0 },
                ));
                bucket_min = f32::INFINITY;
                bucket_max = f32::NEG_INFINITY;
                samples_in_bucket = 0;
            }
        }
    }
    if samples_in_bucket > 0 {
        peaks.push((
            if bucket_min.is_finite() { bucket_min } else { 0.0 },
            if bucket_max.is_finite() { bucket_max } else { 0.0 },
        ));
    }

    Ok(Some(WaveformPeaks { bucket_us, peaks }))
}

pub fn waveform_cache_path(data_dir: &Path, clip_id: uuid::Uuid) -> PathBuf {
    data_dir.join("waveforms").join(format!("{clip_id}.bin"))
}

pub fn load_waveform(path: &Path) -> DecoderResult<WaveformPeaks> {
    WaveformPeaks::load_from(path)
}
