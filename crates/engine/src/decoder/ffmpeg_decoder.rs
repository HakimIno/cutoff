use std::path::Path;
use std::sync::{Arc, Once};

use ffmpeg_next::format::{input, Pixel};
use ffmpeg_next::media::Type;
use ffmpeg_next::software::scaling::{context::Context as Scaler, flag::Flags};
use ffmpeg_next::util::frame::video::Video as VideoFrame;

use super::{DecodedFrame, DecoderError, DecoderResult, MediaDecoder, StreamMeta};

static FFMPEG_INIT: Once = Once::new();

fn init_ffmpeg() {
    FFMPEG_INIT.call_once(|| {
        let _ = ffmpeg_next::init();
    });
}

pub struct FfmpegMediaDecoder {
    ictx: ffmpeg_next::format::context::Input,
    decoder: ffmpeg_next::decoder::Video,
    scaler: Scaler,
    video_stream_index: usize,
    time_base_num: i32,
    time_base_den: i32,
    width: u32,
    height: u32,
    meta: StreamMeta,
    eof: bool,
}

impl FfmpegMediaDecoder {
    pub fn open(path: &Path) -> DecoderResult<Self> {
        init_ffmpeg();

        let ictx = input(&path)?;
        let stream = ictx
            .streams()
            .best(Type::Video)
            .ok_or_else(|| DecoderError::NoVideoStream(path.display().to_string()))?;
        let video_stream_index = stream.index();
        let time_base = stream.time_base();

        let avg_rate = stream.avg_frame_rate();
        let duration_us = if stream.duration() > 0 {
            (stream.duration() as f64 * (time_base.numerator() as f64
                / time_base.denominator().max(1) as f64)
                * 1_000_000.0) as i64
        } else {
            (ictx.duration() as f64 * 1_000_000.0 / ffmpeg_next::ffi::AV_TIME_BASE as f64) as i64
        };

        let codec_ctx = ffmpeg_next::codec::context::Context::from_parameters(stream.parameters())?;
        let mut decoder = codec_ctx.decoder().video()?;
        // ffmpeg frame-threading scales decode throughput nearly linearly up
        // to ~8 threads on H.264/H.265; beyond that the marginal gain drops
        // and latency creeps up (more frames in flight before output).
        let thread_count = num_cpus::get().clamp(2, 8);
        decoder.set_threading(ffmpeg_next::codec::threading::Config::count(thread_count));

        let width = decoder.width();
        let height = decoder.height();
        let scaler = Scaler::get(
            decoder.format(),
            width,
            height,
            Pixel::RGBA,
            width,
            height,
            Flags::BILINEAR,
        )?;

        let meta = StreamMeta {
            duration_us,
            frame_rate_num: avg_rate.numerator(),
            frame_rate_den: avg_rate.denominator(),
        };

        Ok(Self {
            ictx,
            decoder,
            scaler,
            video_stream_index,
            time_base_num: time_base.numerator(),
            time_base_den: time_base.denominator(),
            width,
            height,
            meta,
            eof: false,
        })
    }

    pub fn width(&self) -> u32 {
        self.width
    }
    pub fn height(&self) -> u32 {
        self.height
    }

    fn pts_to_us(&self, pts: i64) -> i64 {
        if self.time_base_den == 0 {
            return 0;
        }
        (pts as f64 * self.time_base_num as f64 / self.time_base_den as f64 * 1_000_000.0) as i64
    }

    fn convert(&mut self, decoded: &VideoFrame, pts_us: i64) -> DecoderResult<DecodedFrame> {
        let mut rgba = VideoFrame::empty();
        self.scaler.run(decoded, &mut rgba)?;
        let stride = rgba.stride(0);
        let row_bytes = (self.width as usize) * 4;
        let h = self.height as usize;
        let src = rgba.data(0);

        // Fast path: ffmpeg's RGBA plane is contiguous (stride == row_bytes),
        // which is the common case → single bulk copy instead of per-row.
        // At 4K this avoids ~2160 separate `copy_from_slice` calls per frame.
        let buf: Vec<u8> = if stride == row_bytes {
            src[..row_bytes * h].to_vec()
        } else {
            let mut buf = vec![0u8; row_bytes * h];
            for y in 0..h {
                let s = y * stride;
                let d = y * row_bytes;
                buf[d..d + row_bytes].copy_from_slice(&src[s..s + row_bytes]);
            }
            buf
        };
        Ok(DecodedFrame {
            width: self.width,
            height: self.height,
            pts_us,
            rgba: Arc::from(buf.into_boxed_slice()),
        })
    }
}

impl MediaDecoder for FfmpegMediaDecoder {
    fn meta(&self) -> StreamMeta {
        self.meta
    }

    fn seek_to_us(&mut self, pts_us: i64) -> DecoderResult<()> {
        // `Input::seek` uses AV_TIME_BASE (microseconds) when stream_index is -1
        // (the default in ffmpeg-next), so pass pts_us directly.
        let target = pts_us.max(0);
        self.ictx.seek(target, ..target)?;
        self.decoder.flush();
        self.eof = false;
        Ok(())
    }

    fn next_frame(&mut self) -> DecoderResult<Option<DecodedFrame>> {
        if self.eof {
            return Ok(None);
        }
        loop {
            // Try to pull a decoded frame already in the decoder.
            let mut frame = VideoFrame::empty();
            match self.decoder.receive_frame(&mut frame) {
                Ok(()) => {
                    let pts = frame.pts().unwrap_or(0);
                    let pts_us = self.pts_to_us(pts);
                    return Ok(Some(self.convert(&frame, pts_us)?));
                }
                Err(ffmpeg_next::Error::Other { errno })
                    if errno == ffmpeg_next::error::EAGAIN => {}
                Err(ffmpeg_next::Error::Eof) => {
                    self.eof = true;
                    return Ok(None);
                }
                Err(e) => return Err(DecoderError::Ffmpeg(e)),
            }

            // Need more input. Pull next packet from container.
            let mut sent_any = false;
            for (stream, packet) in self.ictx.packets() {
                if stream.index() == self.video_stream_index {
                    self.decoder.send_packet(&packet)?;
                    sent_any = true;
                    break;
                }
            }
            if !sent_any {
                self.decoder.send_eof()?;
                self.eof = true;
                // Drain remaining frames on next loop iteration.
                let mut frame = VideoFrame::empty();
                match self.decoder.receive_frame(&mut frame) {
                    Ok(()) => {
                        let pts = frame.pts().unwrap_or(0);
                        let pts_us = self.pts_to_us(pts);
                        return Ok(Some(self.convert(&frame, pts_us)?));
                    }
                    _ => return Ok(None),
                }
            }
        }
    }
}
