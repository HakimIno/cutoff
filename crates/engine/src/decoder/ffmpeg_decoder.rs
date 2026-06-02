use std::path::Path;
use std::sync::{Arc, Once};

use ffmpeg_next::ffi;
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
    /// Lazily built on the first decoded frame, keyed by the source frame's
    /// `(format, width, height)`. Hardware frames are downloaded to NV12/P010
    /// before scaling, so the input format is only known once decoding starts
    /// (and can differ from `decoder.format()` on the hardware path).
    scaler: Option<(Scaler, Pixel, u32, u32)>,
    video_stream_index: usize,
    time_base_num: i32,
    time_base_den: i32,
    width: u32,
    height: u32,
    /// True when VideoToolbox hardware decoding is attached; frames then arrive
    /// in GPU memory and must be transferred to system memory before scaling.
    hw_active: bool,
    /// Last frame produced by `decode_next` (raw — hardware surface or native
    /// pixel format), kept so the caller can decide whether to pay for the
    /// expensive `convert` (GPU download + swscale). Decoding forward to skip
    /// frames is cheap; converting every skipped frame is what isn't.
    cur_frame: VideoFrame,
    cur_pts_us: i64,
    have_cur: bool,
    /// Scratch frame reused for hardware→system memory transfers.
    sw_frame: VideoFrame,
    meta: StreamMeta,
    eof: bool,
}

impl FfmpegMediaDecoder {
    pub fn open(path: &Path) -> DecoderResult<Self> {
        Self::open_scaled(path, 1.0)
    }

    /// Open a decoder whose RGBA output is downscaled by `scale` (0.05..=1.0).
    ///
    /// The downscale happens *inside* swscale, which already runs every frame
    /// to convert the decoder's native pixel format (typically YUV420) to RGBA.
    /// Producing a smaller RGBA plane there is essentially free, yet it shrinks
    /// every downstream cost — the RGBA copy, canvas compositing, channel
    /// transfer, and UI texture upload — by `scale²`. At `scale = 1/3` a 4K
    /// frame drops from ~33 MB to ~3.7 MB, which is what keeps 4K preview
    /// playback smooth. Export still uses [`open`] (full resolution).
    pub fn open_scaled(path: &Path, scale: f32) -> DecoderResult<Self> {
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

        // ffmpeg frame-threading scales decode throughput nearly linearly up
        // to ~8 threads on H.264/H.265; beyond that the marginal gain drops
        // and latency creeps up (more frames in flight before output).
        let thread_count = num_cpus::get().clamp(2, 8);
        let parameters = stream.parameters();

        // Prefer hardware (VideoToolbox) decoding; on any failure fall back to a
        // fresh software context so we never refuse a file we *could* decode.
        let (decoder, hw_active) = match build_video_decoder(parameters.clone(), thread_count, true) {
            Ok(pair) => pair,
            Err(e) => {
                tracing::warn!("hardware decoder open failed ({e}); using software");
                build_video_decoder(parameters, thread_count, false)?
            }
        };

        // Coded dimensions feed the scaler input; the RGBA output is optionally
        // downscaled to keep even dimensions (swscale prefers even sizes for
        // chroma subsampling). `width`/`height` track the *output* size, since
        // that is what `convert` copies and emits. The scaler itself is built
        // lazily once the first frame reveals its true pixel format.
        let src_w = decoder.width();
        let src_h = decoder.height();
        let scale = scale.clamp(0.05, 1.0);
        let (width, height) = if scale >= 0.999 {
            (src_w, src_h)
        } else {
            (
                (((src_w as f32 * scale).round() as u32).max(2)) & !1,
                (((src_h as f32 * scale).round() as u32).max(2)) & !1,
            )
        };

        let meta = StreamMeta {
            duration_us,
            frame_rate_num: avg_rate.numerator(),
            frame_rate_den: avg_rate.denominator(),
        };

        Ok(Self {
            ictx,
            decoder,
            scaler: None,
            video_stream_index,
            time_base_num: time_base.numerator(),
            time_base_den: time_base.denominator(),
            width,
            height,
            hw_active,
            cur_frame: VideoFrame::empty(),
            cur_pts_us: 0,
            have_cur: false,
            sw_frame: VideoFrame::empty(),
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

    /// Whether VideoToolbox hardware decoding is attached (macOS). Useful for
    /// diagnostics; software decode is used transparently when this is false.
    pub fn is_hardware(&self) -> bool {
        self.hw_active
    }

    fn pts_to_us(&self, pts: i64) -> i64 {
        if self.time_base_den == 0 {
            return 0;
        }
        (pts as f64 * self.time_base_num as f64 / self.time_base_den as f64 * 1_000_000.0) as i64
    }

    /// Decode the next frame into `cur_frame` **without** converting it to RGBA,
    /// returning its presentation timestamp (µs). This is the cheap half of
    /// decoding: for hardware it leaves the surface on the GPU. Callers skipping
    /// frames (e.g. seeking forward to a target time) loop on this and only call
    /// [`convert_current`](Self::convert_current) for the frame they keep,
    /// avoiding a GPU download + swscale per discarded frame.
    pub fn decode_next(&mut self) -> DecoderResult<Option<i64>> {
        if self.eof {
            return Ok(None);
        }
        loop {
            let mut frame = VideoFrame::empty();
            match self.decoder.receive_frame(&mut frame) {
                Ok(()) => {
                    let pts_us = self.pts_to_us(frame.pts().unwrap_or(0));
                    self.cur_frame = frame;
                    self.cur_pts_us = pts_us;
                    self.have_cur = true;
                    return Ok(Some(pts_us));
                }
                Err(ffmpeg_next::Error::Other { errno })
                    if errno == ffmpeg_next::error::EAGAIN => {}
                Err(ffmpeg_next::Error::Eof) => {
                    self.eof = true;
                    return Ok(None);
                }
                Err(e) => return Err(DecoderError::Ffmpeg(e)),
            }

            // Need more input. Pull the next video packet from the container.
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
                let mut frame = VideoFrame::empty();
                match self.decoder.receive_frame(&mut frame) {
                    Ok(()) => {
                        let pts_us = self.pts_to_us(frame.pts().unwrap_or(0));
                        self.cur_frame = frame;
                        self.cur_pts_us = pts_us;
                        self.have_cur = true;
                        return Ok(Some(pts_us));
                    }
                    _ => return Ok(None),
                }
            }
        }
    }

    /// Convert the frame last produced by [`decode_next`](Self::decode_next) to
    /// RGBA, downloading from the GPU first on the hardware path. Errors if no
    /// frame has been decoded yet.
    pub fn convert_current(&mut self) -> DecoderResult<DecodedFrame> {
        if !self.have_cur {
            return Err(DecoderError::Ffmpeg(ffmpeg_next::Error::from(
                ffmpeg_next::error::EAGAIN,
            )));
        }
        let pts_us = self.cur_pts_us;
        let is_hw = self.hw_active
            && unsafe { (*self.cur_frame.as_ptr()).format }
                == ffi::AVPixelFormat::AV_PIX_FMT_VIDEOTOOLBOX as i32;
        if is_hw {
            // Download the GPU surface to system memory (typically NV12/P010).
            // The mem::swap dance reuses `sw_frame` while satisfying the borrow
            // checker (we need `&cur_frame`/`&sw_frame` plus `&mut self.scaler`).
            let mut sw = std::mem::replace(&mut self.sw_frame, VideoFrame::empty());
            let ret = unsafe {
                ffi::av_hwframe_transfer_data(sw.as_mut_ptr(), self.cur_frame.as_ptr(), 0)
            };
            if ret < 0 {
                self.sw_frame = sw;
                return Err(DecoderError::Ffmpeg(ffmpeg_next::Error::from(ret)));
            }
            let out = self.convert(&sw, pts_us);
            self.sw_frame = sw;
            out
        } else {
            let cur = std::mem::replace(&mut self.cur_frame, VideoFrame::empty());
            let out = self.convert(&cur, pts_us);
            self.cur_frame = cur;
            out
        }
    }

    fn convert(&mut self, decoded: &VideoFrame, pts_us: i64) -> DecoderResult<DecodedFrame> {
        // (Re)build the scaler when the source format/size differs from the
        // cached one — hardware downloads can surface a format that differs
        // from `decoder.format()`, and only the real frame reveals it.
        let src_fmt = decoded.format();
        let src_w = decoded.width();
        let src_h = decoded.height();
        let stale = match &self.scaler {
            Some((_, f, w, h)) => *f != src_fmt || *w != src_w || *h != src_h,
            None => true,
        };
        if stale {
            let scaler = Scaler::get(
                src_fmt,
                src_w,
                src_h,
                Pixel::RGBA,
                self.width,
                self.height,
                Flags::FAST_BILINEAR,
            )?;
            self.scaler = Some((scaler, src_fmt, src_w, src_h));
        }
        let scaler = &mut self.scaler.as_mut().expect("scaler initialized above").0;

        let mut rgba = VideoFrame::empty();
        scaler.run(decoded, &mut rgba)?;
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
        self.have_cur = false;
        Ok(())
    }

    fn next_frame(&mut self) -> DecoderResult<Option<DecodedFrame>> {
        match self.decode_next()? {
            Some(_) => Ok(Some(self.convert_current()?)),
            None => Ok(None),
        }
    }
}

/// Build (but do not yet decode with) a video decoder from stream parameters,
/// optionally attaching VideoToolbox hardware acceleration. Returns the opened
/// decoder and whether hardware acceleration was successfully attached.
fn build_video_decoder(
    parameters: ffmpeg_next::codec::Parameters,
    thread_count: usize,
    try_hw: bool,
) -> DecoderResult<(ffmpeg_next::decoder::Video, bool)> {
    let codec_ctx = ffmpeg_next::codec::context::Context::from_parameters(parameters)?;
    let mut decoder_ctx = codec_ctx.decoder();
    // Threading must be configured before the context is opened to take effect.
    decoder_ctx.set_threading(ffmpeg_next::codec::threading::Config::count(thread_count));

    let hw_active = if try_hw {
        unsafe { attach_videotoolbox(decoder_ctx.as_mut_ptr()) }
    } else {
        false
    };

    let decoder = decoder_ctx.video()?;
    Ok((decoder, hw_active))
}

/// Attach a VideoToolbox hardware device to the codec context (macOS only).
/// Returns `true` on success. The device buffer reference is handed to the
/// codec context, which unrefs it when the context is freed.
///
/// # Safety
/// `ctx` must be a valid, not-yet-opened `AVCodecContext` pointer.
#[cfg(target_os = "macos")]
unsafe fn attach_videotoolbox(ctx: *mut ffi::AVCodecContext) -> bool {
    let mut device: *mut ffi::AVBufferRef = std::ptr::null_mut();
    let ret = ffi::av_hwdevice_ctx_create(
        &mut device,
        ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_VIDEOTOOLBOX,
        std::ptr::null(),
        std::ptr::null_mut(),
        0,
    );
    if ret < 0 || device.is_null() {
        return false;
    }
    (*ctx).hw_device_ctx = device;
    (*ctx).get_format = Some(get_hw_format);
    true
}

#[cfg(not(target_os = "macos"))]
unsafe fn attach_videotoolbox(_ctx: *mut ffi::AVCodecContext) -> bool {
    false
}

/// `get_format` callback: pick the VideoToolbox surface format when the decoder
/// offers it, otherwise fall back to the first (software) format so decoding
/// still succeeds on codecs VideoToolbox cannot handle.
///
/// # Safety
/// Called by FFmpeg with a valid `AV_PIX_FMT_NONE`-terminated format list.
unsafe extern "C" fn get_hw_format(
    _ctx: *mut ffi::AVCodecContext,
    fmts: *const ffi::AVPixelFormat,
) -> ffi::AVPixelFormat {
    // First offered format is the software fallback (or NONE for an empty list).
    let fallback = *fmts;
    let mut p = fmts;
    while *p != ffi::AVPixelFormat::AV_PIX_FMT_NONE {
        if *p == ffi::AVPixelFormat::AV_PIX_FMT_VIDEOTOOLBOX {
            return ffi::AVPixelFormat::AV_PIX_FMT_VIDEOTOOLBOX;
        }
        p = p.add(1);
    }
    fallback
}
