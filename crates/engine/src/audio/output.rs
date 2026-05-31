//! cpal-driven audio output with a lock-free producer/consumer ring buffer.
//!
//! The preview thread feeds f32 stereo samples into the ring; the cpal
//! callback drains it onto the device. Underflows (consumer ahead of
//! producer) are filled with silence so the device never stalls.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Sample as CpalSample, SampleFormat, Stream, StreamConfig};
use ringbuf::traits::{Consumer as ConsumerTrait, Split};
use ringbuf::{HeapCons as Consumer, HeapProd as Producer, HeapRb};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

use super::{CHANNELS, SAMPLE_RATE};

/// Capacity of the audio ring buffer in samples (≈ 250 ms of stereo @ 48k).
const RING_CAPACITY_SAMPLES: usize = 24_000;

#[allow(dead_code)]
struct SendStream(Stream);
unsafe impl Send for SendStream {}
unsafe impl Sync for SendStream {}

/// Live audio output. Holds the cpal stream so it stays alive; dropping
/// stops playback. The `producer` is moved into the preview thread that
/// decodes audio.
pub struct AudioOutput {
    _stream: SendStream,
    pub producer: Producer<f32>,
    /// Set by the cpal callback every time it consumes samples — gives the
    /// preview thread an approximate audio clock for A/V sync. Unit: frames
    /// (one frame = 2 samples for stereo).
    pub frames_played: Arc<AtomicU32>,
    pub muted: Arc<AtomicBool>,
    pub volume_milli: Arc<AtomicU32>, // 1000 = 100 %
}

#[derive(Debug, thiserror::Error)]
pub enum AudioOutputError {
    #[error("no default audio output device available")]
    NoDevice,
    #[error("could not negotiate stream config: {0}")]
    Config(String),
    #[error("cpal stream error: {0}")]
    Stream(String),
}

impl AudioOutput {
    pub fn new() -> Result<Self, AudioOutputError> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or(AudioOutputError::NoDevice)?;

        // Prefer the device's default config; fall back to our canonical
        // 48k/stereo if the negotiated rate matches. We don't resample on
        // output — the decoder already targets SAMPLE_RATE.
        let default_cfg = device
            .default_output_config()
            .map_err(|e| AudioOutputError::Config(e.to_string()))?;

        let sample_format = default_cfg.sample_format();
        let config: StreamConfig = StreamConfig {
            channels: CHANNELS,
            sample_rate: cpal::SampleRate(SAMPLE_RATE),
            buffer_size: cpal::BufferSize::Default,
        };

        let rb = HeapRb::<f32>::new(RING_CAPACITY_SAMPLES);
        let (producer, consumer) = rb.split();

        let frames_played = Arc::new(AtomicU32::new(0));
        let muted = Arc::new(AtomicBool::new(false));
        let volume_milli = Arc::new(AtomicU32::new(1000));

        let frames_played_cb = frames_played.clone();
        let muted_cb = muted.clone();
        let vol_cb = volume_milli.clone();

        let stream = match sample_format {
            SampleFormat::F32 => build_stream::<f32>(
                &device,
                &config,
                consumer,
                frames_played_cb,
                muted_cb,
                vol_cb,
                |v| v,
            )?,
            SampleFormat::I16 => build_stream::<i16>(
                &device,
                &config,
                consumer,
                frames_played_cb,
                muted_cb,
                vol_cb,
                |v| (v * i16::MAX as f32).clamp(i16::MIN as f32, i16::MAX as f32) as i16,
            )?,
            SampleFormat::U16 => build_stream::<u16>(
                &device,
                &config,
                consumer,
                frames_played_cb,
                muted_cb,
                vol_cb,
                |v| {
                    let mid = (u16::MAX as f32) * 0.5;
                    (mid + v * mid).clamp(0.0, u16::MAX as f32) as u16
                },
            )?,
            other => {
                return Err(AudioOutputError::Config(format!(
                    "unsupported sample format: {other:?}"
                )))
            }
        };

        stream
            .play()
            .map_err(|e| AudioOutputError::Stream(e.to_string()))?;

        Ok(Self {
            _stream: SendStream(stream),
            producer,
            frames_played,
            muted,
            volume_milli,
        })
    }

    pub fn set_muted(&self, muted: bool) {
        self.muted.store(muted, Ordering::Relaxed);
    }

    pub fn set_volume(&self, v: f32) {
        let v = v.clamp(0.0, 2.0);
        self.volume_milli
            .store((v * 1000.0) as u32, Ordering::Relaxed);
    }

    /// Best-effort: number of stereo frames consumed since the stream was
    /// created. Wraps at u32::MAX after ~25 hours @ 48k — acceptable.
    pub fn frames_played(&self) -> u32 {
        self.frames_played.load(Ordering::Relaxed)
    }

    /// Clone the shared frames-played counter so another thread can read the
    /// audio clock without holding a reference to the `!Send` stream.
    pub fn frames_played_handle(&self) -> Arc<AtomicU32> {
        self.frames_played.clone()
    }
}

fn build_stream<T>(
    device: &cpal::Device,
    config: &StreamConfig,
    mut consumer: Consumer<f32>,
    frames_played: Arc<AtomicU32>,
    muted: Arc<AtomicBool>,
    volume_milli: Arc<AtomicU32>,
    convert: fn(f32) -> T,
) -> Result<Stream, AudioOutputError>
where
    T: CpalSample + cpal::SizedSample + Send + 'static,
{
    let err_fn = |err| tracing::warn!(error = %err, "cpal output stream error");
    device
        .build_output_stream(
            config,
            move |out: &mut [T], _info| {
                let muted_now = muted.load(Ordering::Relaxed);
                let vol = volume_milli.load(Ordering::Relaxed) as f32 / 1000.0;
                for chunk in out.chunks_mut(CHANNELS as usize) {
                    let mut sample_l = 0.0f32;
                    let mut sample_r = 0.0f32;
                    if let Some(l) = consumer.try_pop() {
                        sample_l = l;
                    }
                    if let Some(r) = consumer.try_pop() {
                        sample_r = r;
                    }
                    if muted_now {
                        sample_l = 0.0;
                        sample_r = 0.0;
                    } else {
                        sample_l *= vol;
                        sample_r *= vol;
                    }
                    chunk[0] = convert(sample_l);
                    if chunk.len() > 1 {
                        chunk[1] = convert(sample_r);
                    }
                }
                let frames = (out.len() / CHANNELS as usize) as u32;
                frames_played.fetch_add(frames, Ordering::Relaxed);
            },
            err_fn,
            None,
        )
        .map_err(|e| AudioOutputError::Stream(e.to_string()))
}
