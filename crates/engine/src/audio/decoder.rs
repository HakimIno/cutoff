//! Audio decode → interleaved f32 stereo @ 48kHz.
//!
//! Uses ffmpeg-next's software resampler (libswresample) to normalize to
//! the canonical preview format defined in [`super::SAMPLE_RATE`] /
//! [`super::CHANNELS`]. The output is interleaved L,R,L,R,... so a single
//! `Vec<f32>` can be pushed directly into cpal's ring buffer.

use std::path::Path;
use std::sync::Once;

use ffmpeg_next::format::{input, sample::Type as SampleType, Sample};
use ffmpeg_next::media::Type;
use ffmpeg_next::software::resampling::Context as Resampler;
use ffmpeg_next::util::channel_layout::ChannelLayout;
use ffmpeg_next::util::frame::audio::Audio as AudioRawFrame;

use super::{CHANNELS, SAMPLE_RATE};
use crate::decoder::DecoderResult;

static FFMPEG_INIT: Once = Once::new();
fn init_ffmpeg() {
    FFMPEG_INIT.call_once(|| {
        let _ = ffmpeg_next::init();
    });
}

#[derive(Debug, Clone)]
pub struct AudioFrame {
    /// Interleaved L,R,L,R,...
    pub samples: Vec<f32>,
    /// Presentation timestamp in microseconds (source-time, pre-trim).
    pub pts_us: i64,
}

pub struct FfmpegAudioDecoder {
    ictx: ffmpeg_next::format::context::Input,
    decoder: ffmpeg_next::decoder::Audio,
    resampler: Resampler,
    stream_index: usize,
    time_base_num: i32,
    time_base_den: i32,
    duration_us: i64,
    eof: bool,
}

impl FfmpegAudioDecoder {
    /// Returns `Ok(None)` when the source has no audio stream — that's a
    /// normal condition for silent footage, not an error.
    pub fn open(path: &Path) -> DecoderResult<Option<Self>> {
        init_ffmpeg();
        let ictx = input(&path)?;
        let stream = match ictx.streams().best(Type::Audio) {
            Some(s) => s,
            None => return Ok(None),
        };
        let stream_index = stream.index();
        let time_base = stream.time_base();
        let duration_us = if stream.duration() > 0 {
            (stream.duration() as f64 * time_base.numerator() as f64
                / time_base.denominator().max(1) as f64
                * 1_000_000.0) as i64
        } else {
            (ictx.duration() as f64 * 1_000_000.0
                / ffmpeg_next::ffi::AV_TIME_BASE as f64) as i64
        };

        let codec_ctx = ffmpeg_next::codec::context::Context::from_parameters(stream.parameters())?;
        let decoder = codec_ctx.decoder().audio()?;

        let in_format = decoder.format();
        let in_rate = decoder.rate();
        let in_layout = decoder.channel_layout();

        let resampler = Resampler::get(
            in_format,
            in_layout,
            in_rate,
            Sample::F32(SampleType::Packed),
            ChannelLayout::STEREO,
            SAMPLE_RATE,
        )?;

        Ok(Some(Self {
            ictx,
            decoder,
            resampler,
            stream_index,
            time_base_num: time_base.numerator(),
            time_base_den: time_base.denominator(),
            duration_us,
            eof: false,
        }))
    }

    pub fn duration_us(&self) -> i64 {
        self.duration_us
    }

    fn pts_to_us(&self, pts: i64) -> i64 {
        if self.time_base_den == 0 {
            return 0;
        }
        (pts as f64 * self.time_base_num as f64 / self.time_base_den as f64 * 1_000_000.0) as i64
    }

    pub fn seek_to_us(&mut self, pts_us: i64) -> DecoderResult<()> {
        let target = pts_us.max(0);
        self.ictx.seek(target, ..target)?;
        self.decoder.flush();
        self.eof = false;
        Ok(())
    }

    /// Pull the next decoded audio frame, resampled to 48k stereo f32.
    /// Returns `Ok(None)` at EOF.
    pub fn next_frame(&mut self) -> DecoderResult<Option<AudioFrame>> {
        if self.eof {
            return Ok(None);
        }
        loop {
            let mut frame = AudioRawFrame::empty();
            match self.decoder.receive_frame(&mut frame) {
                Ok(()) => {
                    let pts = frame.pts().unwrap_or(0);
                    let pts_us = self.pts_to_us(pts);
                    let samples = self.resample_to_vec(&frame)?;
                    return Ok(Some(AudioFrame { samples, pts_us }));
                }
                Err(ffmpeg_next::Error::Other { errno })
                    if errno == ffmpeg_next::error::EAGAIN => {}
                Err(ffmpeg_next::Error::Eof) => {
                    self.eof = true;
                    return Ok(None);
                }
                Err(e) => return Err(e.into()),
            }

            // Need more input. Pull next audio packet.
            let mut sent_any = false;
            for (stream, packet) in self.ictx.packets() {
                if stream.index() == self.stream_index {
                    self.decoder.send_packet(&packet)?;
                    sent_any = true;
                    break;
                }
            }
            if !sent_any {
                self.decoder.send_eof()?;
                self.eof = true;
            }
        }
    }

    fn resample_to_vec(&mut self, frame: &AudioRawFrame) -> DecoderResult<Vec<f32>> {
        let mut resampled = AudioRawFrame::empty();
        self.resampler.run(frame, &mut resampled)?;
        // Interleaved F32: data(0) is the packed sample plane.
        let bytes = resampled.data(0);
        let n_samples = resampled.samples() * CHANNELS as usize;
        let mut out = Vec::with_capacity(n_samples);
        // Safety: F32 packed format means data(0) is `n_samples * 4` bytes of native-endian f32.
        for i in 0..n_samples {
            let off = i * 4;
            let bits = u32::from_ne_bytes([
                bytes[off],
                bytes[off + 1],
                bytes[off + 2],
                bytes[off + 3],
            ]);
            out.push(f32::from_bits(bits));
        }
        Ok(out)
    }
}
