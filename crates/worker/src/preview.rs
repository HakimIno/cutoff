//! Background preview-playback threads.
//!
//! Each preview session owns:
//!   * a **render thread** (producer) that decodes + composites timeline frames
//!     ahead of playback and pushes them into a bounded picture queue;
//!   * a **present thread** (consumer) that paces delivery of those frames to
//!     the UI using the audio clock (or wall-clock fallback) — it never decodes,
//!     so its cadence stays steady even when per-frame decode time jitters;
//!   * an **audio thread** (optional) that
//!     owns the `!Send` cpal `Stream` and feeds the ring buffer continuously.
//!
//! Splitting decode (render) from present is what keeps playback at a steady
//! frame rate: the bounded queue absorbs decode-time variance, and a shared
//! generation counter discards frames rendered for a position that a seek or
//! project change has since abandoned.
//!
//! Coordination is by `std::sync::mpsc` control channels, the bounded picture
//! `sync_channel`, a shared `Arc<AtomicU64>` picture generation, and the shared
//! `Arc<AtomicU32>` audio clock from `AudioOutput`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc as std_mpsc;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use ringbuf::traits::{Observer, Producer};
use tokio::sync::mpsc as tokio_mpsc;
use uuid::Uuid;
use video_merger_core::domain::{Project, TrackClip};
use video_merger_engine::audio::output::AudioOutput;
use video_merger_engine::audio::FfmpegAudioDecoder;
use video_merger_engine::decoder::{FfmpegMediaDecoder, MediaDecoder, DecodedFrame};

use crate::job::Event;

/// Longest preview-canvas side, in pixels. Source frames whose longest side
/// exceeds this are decoded+composited downscaled so 4K/8K timelines stay
/// smooth; everything the user sees is bounded by the preview panel anyway.
/// Export is unaffected — it renders at full resolution.
const PREVIEW_MAX_DIM: u32 = 1280;

/// Downscale factor (≤ 1.0) that fits a `w×h` source within [`PREVIEW_MAX_DIM`].
fn preview_scale_for(w: u32, h: u32) -> f32 {
    let longest = w.max(h);
    if longest > PREVIEW_MAX_DIM {
        PREVIEW_MAX_DIM as f32 / longest as f32
    } else {
        1.0
    }
}

/// Apply `scale` to a dimension, clamped to an even value ≥ 2 (matches the
/// even-size rounding the decoder uses, so canvas and frames stay aligned).
fn scaled_dim(v: u32, scale: f32) -> u32 {
    (((v as f32 * scale).round() as u32).max(2)) & !1
}

#[derive(Debug)]
pub enum PreviewCtrl {
    Play,
    Pause,
    Seek { pts_us: i64 },
    Stop,
    SetMuted(bool),
    SetVolume(f32),
    UpdateProject(Project),
}

/// How many decoded/composited frames the render thread may stay ahead of the
/// present thread. This bounded buffer is what absorbs per-frame decode jitter
/// (a slow keyframe-distance decode followed by fast ones) so the present
/// thread can emit a steady cadence — at the cost of `DEPTH × frame_interval`
/// of latency (~100–200ms at 30–60fps), which a bounded queue keeps in check.
const PICTURE_QUEUE_DEPTH: usize = 6;

// ── Internal per-thread message types ─────────────────────────────────────

/// Control messages for the **render** (producer) thread.
enum RenderCtrl {
    Play,
    Pause,
    /// Jump the render head to `pts_us` and adopt picture-generation `gen`
    /// (frames pushed afterwards are tagged with it). Renders one frame at the
    /// new position immediately so paused scrubbing stays responsive.
    Seek { pts_us: i64, gen: u64 },
    Stop,
    UpdateProject { project: Project, gen: u64 },
}

/// Control messages for the **present** (consumer/pacing) thread.
enum PresentCtrl {
    Play,
    Pause,
    Seek { pts_us: i64, gen: u64 },
    Stop,
    UpdateProject { project: Project, gen: u64 },
}

/// A composited timeline frame in the render→present picture queue, tagged with
/// the picture generation it belongs to. A seek/project-change bumps the shared
/// generation; the present thread drops any item whose generation no longer
/// matches, so frames rendered for a now-abandoned position never reach screen.
struct PictureItem {
    gen: u64,
    frame: DecodedFrame,
}

enum AudioCtrl {
    Play,
    Pause,
    Seek(i64),
    Stop,
    SetMuted(bool),
    SetVolume(f32),
    UpdateProject(Project),
}

pub struct PreviewHandle {
    render_tx: std_mpsc::Sender<RenderCtrl>,
    present_tx: std_mpsc::Sender<PresentCtrl>,
    audio_tx: Option<std_mpsc::Sender<AudioCtrl>>,
    /// Shared picture-generation counter. Bumped here (before the control
    /// messages are sent) on every seek/project change so both downstream
    /// threads adopt the same new generation and stale in-flight frames are
    /// dropped. See [`PictureItem`].
    generation: Arc<AtomicU64>,
    _render_join: thread::JoinHandle<()>,
    _present_join: thread::JoinHandle<()>,
    _audio_join: Option<thread::JoinHandle<()>>,
}

impl PreviewHandle {
    pub fn send(&self, ctrl: PreviewCtrl) {
        match ctrl {
            PreviewCtrl::Play => {
                let _ = self.render_tx.send(RenderCtrl::Play);
                let _ = self.present_tx.send(PresentCtrl::Play);
                if let Some(tx) = &self.audio_tx {
                    let _ = tx.send(AudioCtrl::Play);
                }
            }
            PreviewCtrl::Pause => {
                let _ = self.render_tx.send(RenderCtrl::Pause);
                let _ = self.present_tx.send(PresentCtrl::Pause);
                if let Some(tx) = &self.audio_tx {
                    let _ = tx.send(AudioCtrl::Pause);
                }
            }
            PreviewCtrl::Seek { pts_us } => {
                // Bump the generation *before* notifying the threads so both see
                // the new value and discard frames from the old position.
                let gen = self.generation.fetch_add(1, Ordering::AcqRel) + 1;
                let _ = self.render_tx.send(RenderCtrl::Seek { pts_us, gen });
                let _ = self.present_tx.send(PresentCtrl::Seek { pts_us, gen });
                if let Some(tx) = &self.audio_tx {
                    let _ = tx.send(AudioCtrl::Seek(pts_us));
                }
            }
            PreviewCtrl::Stop => {
                let _ = self.render_tx.send(RenderCtrl::Stop);
                let _ = self.present_tx.send(PresentCtrl::Stop);
                if let Some(tx) = &self.audio_tx {
                    let _ = tx.send(AudioCtrl::Stop);
                }
            }
            PreviewCtrl::SetMuted(m) => {
                if let Some(tx) = &self.audio_tx {
                    let _ = tx.send(AudioCtrl::SetMuted(m));
                }
            }
            PreviewCtrl::SetVolume(v) => {
                if let Some(tx) = &self.audio_tx {
                    let _ = tx.send(AudioCtrl::SetVolume(v));
                }
            }
            PreviewCtrl::UpdateProject(project) => {
                let gen = self.generation.fetch_add(1, Ordering::AcqRel) + 1;
                let _ = self
                    .render_tx
                    .send(RenderCtrl::UpdateProject { project: project.clone(), gen });
                let _ = self
                    .present_tx
                    .send(PresentCtrl::UpdateProject { project: project.clone(), gen });
                if let Some(tx) = &self.audio_tx {
                    let _ = tx.send(AudioCtrl::UpdateProject(project));
                }
            }
        }
    }
}

/// Spawn the preview threads, returning the control handle.
pub fn spawn_preview(
    project: Project,
    event_tx: tokio_mpsc::UnboundedSender<Event>,
) -> PreviewHandle {
    let (audio_tx, audio_join, audio_clock) = match AudioOutput::new() {
        Ok(output) => {
            let clock = output.frames_played_handle();
            let (tx, rx) = std_mpsc::channel::<AudioCtrl>();
            let proj_a = project.clone();
            let join = thread::Builder::new()
                .name("cutoff-preview-audio".into())
                .spawn(move || audio_loop(proj_a, output, rx))
                .expect("audio preview thread spawn");
            (Some(tx), Some(join), Some(clock))
        }
        Err(e) => {
            tracing::warn!(error = %e, "audio output unavailable");
            (None, None, None)
        }
    };

    let generation = Arc::new(AtomicU64::new(0));

    // Bounded picture queue: render (producer) → present (consumer). A full
    // queue back-pressures the render thread so it stays at most
    // `PICTURE_QUEUE_DEPTH` frames ahead instead of decoding unboundedly.
    let (pic_tx, pic_rx) = std_mpsc::sync_channel::<PictureItem>(PICTURE_QUEUE_DEPTH);

    let (render_tx, render_rx) = std_mpsc::channel::<RenderCtrl>();
    let (present_tx, present_rx) = std_mpsc::channel::<PresentCtrl>();

    // The present thread can ask the render thread to resync (jump forward to
    // the audio clock) when decode falls behind, so it holds a render sender.
    let render_tx_for_present = render_tx.clone();

    let proj_r = project.clone();
    let gen_r = generation.clone();
    let render_join = thread::Builder::new()
        .name("cutoff-preview-render".into())
        .spawn(move || render_loop(proj_r, render_rx, pic_tx, gen_r))
        .expect("render preview thread spawn");

    let proj_p = project;
    let gen_p = generation.clone();
    let present_join = thread::Builder::new()
        .name("cutoff-preview-present".into())
        .spawn(move || {
            present_loop(proj_p, present_rx, pic_rx, render_tx_for_present, event_tx, audio_clock, gen_p);
        })
        .expect("present preview thread spawn");

    PreviewHandle {
        render_tx,
        present_tx,
        audio_tx,
        generation,
        _render_join: render_join,
        _present_join: present_join,
        _audio_join: audio_join,
    }
}

// ── Decoder Wrapping structures ──────────────────────────────────────────

struct DecoderState {
    decoder: FfmpegMediaDecoder,
    last_pts_us: Option<i64>,
}

impl DecoderState {
    fn new(decoder: FfmpegMediaDecoder) -> Self {
        Self {
            decoder,
            last_pts_us: None,
        }
    }

    fn get_frame_at(&mut self, target_us: i64) -> Option<DecodedFrame> {
        let threshold_us = 200_000; // 200ms

        let needs_seek = match self.last_pts_us {
            None => true,
            Some(last) => target_us < last || target_us - last > threshold_us,
        };

        if needs_seek {
            if let Err(e) = self.decoder.seek_to_us(target_us) {
                tracing::warn!("Seek failed: {}", e);
                return None;
            }
            self.last_pts_us = None;
        }

        // Decode forward cheaply (no GPU download / swscale) until we reach the
        // target time, then convert only the frame we actually display. This is
        // what lets preview keep up with high-fps 4K sources: skipped frames
        // cost a bare decode, not a full RGBA conversion.
        loop {
            match self.decoder.decode_next() {
                Ok(Some(pts_us)) => {
                    self.last_pts_us = Some(pts_us);
                    if pts_us >= target_us {
                        return self.decoder.convert_current().ok();
                    }
                }
                // EOF: show the last decoded frame (e.g. the tail of a clip).
                Ok(None) => return self.decoder.convert_current().ok(),
                Err(e) => {
                    tracing::warn!("Decode frame failed: {}", e);
                    return None;
                }
            }
        }
    }
}

struct AudioDecoderState {
    decoder: FfmpegAudioDecoder,
    buffer: Vec<f32>,
    current_time_us: i64,
}

impl AudioDecoderState {
    fn new(decoder: FfmpegAudioDecoder, initial_time_us: i64) -> Self {
        Self {
            decoder,
            buffer: Vec::new(),
            current_time_us: initial_time_us,
        }
    }

    fn seek_to(&mut self, timeline_time_us: i64, clip_start_us: i64, clip_trim_in_us: i64) {
        let target_src_us = timeline_time_us - clip_start_us + clip_trim_in_us;
        let target_src_us = target_src_us.max(0);
        if let Err(e) = self.decoder.seek_to_us(target_src_us) {
            tracing::warn!("Audio seek failed: {}", e);
        }
        self.buffer.clear();
        self.current_time_us = timeline_time_us;
    }

    fn read_samples(
        &mut self,
        n_samples: usize,
        clip_start_us: i64,
        clip_trim_in_us: i64,
        clip_trim_out_us: i64,
    ) -> Vec<f32> {
        let sample_duration_us = 1_000_000.0 / 48000.0;
        
        while self.buffer.len() < n_samples * 2 {
            let current_src_us = self.current_time_us - clip_start_us + clip_trim_in_us;
            if current_src_us >= clip_trim_out_us {
                break;
            }
            
            match self.decoder.next_frame() {
                Ok(Some(frame)) => {
                    self.buffer.extend_from_slice(&frame.samples);
                }
                Ok(None) => break,
                Err(e) => {
                    tracing::warn!("Audio decode failed: {}", e);
                    break;
                }
            }
        }
        
        let to_take = (n_samples * 2).min(self.buffer.len());
        let mut samples = self.buffer.drain(..to_take).collect::<Vec<f32>>();
        
        if samples.len() < n_samples * 2 {
            samples.resize(n_samples * 2, 0.0);
        }
        
        self.current_time_us += (n_samples as f64 * sample_duration_us) as i64;
        samples
    }
}

// ── Affine Compositing ─────────────────────────────────────────────────────

/// Composite one source frame onto `base` using a full affine transform
/// (crop → per-axis scale → flip → rotate around center → translate), via
/// inverse mapping with nearest-neighbour sampling.
///
/// Geometry matches the export filter graph in `reencode.rs`:
///   * the clip is centered on the canvas, then shifted by `position_*`
///     (already converted to canvas pixels by the caller);
///   * positive `rotation_deg` rotates clockwise on the y-down screen.
///
/// Only the transformed clip's axis-aligned bounding box is iterated, so cost
/// stays proportional to the clip's on-screen size rather than the canvas.
#[allow(clippy::too_many_arguments)]
fn composite_affine(
    base: &mut [u8],
    base_w: usize,
    base_h: usize,
    src: &[u8],
    src_w: usize,
    src_h: usize,
    // Crop in *source-frame* pixels (already scaled to preview resolution).
    crop_l: usize,
    crop_t: usize,
    crop_r: usize,
    crop_b: usize,
    scale_x: f32,
    scale_y: f32,
    rotation_deg: f32,
    flip_h: bool,
    flip_v: bool,
    opacity: f32,
    // Clip-center position on the canvas, in canvas pixels.
    center_x: f32,
    center_y: f32,
) {
    if opacity <= 0.0 {
        return;
    }
    // Cropped source region.
    let csw = src_w.saturating_sub(crop_l + crop_r);
    let csh = src_h.saturating_sub(crop_t + crop_b);
    if csw == 0 || csh == 0 || scale_x.abs() < 1e-6 || scale_y.abs() < 1e-6 {
        return;
    }

    let theta = rotation_deg.to_radians();
    let (sin_t, cos_t) = theta.sin_cos();

    // On-canvas half-extents of the unrotated, scaled clip.
    let hw = csw as f32 * scale_x.abs() / 2.0;
    let hh = csh as f32 * scale_y.abs() / 2.0;

    // Axis-aligned bounding box of the rotated rectangle.
    let ext_x = hw * cos_t.abs() + hh * sin_t.abs();
    let ext_y = hw * sin_t.abs() + hh * cos_t.abs();
    let min_x = ((center_x - ext_x).floor() as i32).max(0);
    let max_x = ((center_x + ext_x).ceil() as i32).min(base_w as i32);
    let min_y = ((center_y - ext_y).floor() as i32).max(0);
    let max_y = ((center_y + ext_y).ceil() as i32).min(base_h as i32);

    let inv_sx = 1.0 / scale_x;
    let inv_sy = 1.0 / scale_y;

    for dy in min_y..max_y {
        let v = dy as f32 + 0.5 - center_y;
        let dest_row = (dy as usize) * base_w * 4;
        for dx in min_x..max_x {
            let u = dx as f32 + 0.5 - center_x;
            // Inverse rotation (by -theta).
            let ur = u * cos_t + v * sin_t;
            let vr = -u * sin_t + v * cos_t;
            // Inverse scale.
            let mut au = ur * inv_sx;
            let mut av = vr * inv_sy;
            // Inverse flip.
            if flip_h { au = -au; }
            if flip_v { av = -av; }
            // Cropped-source pixel.
            let sx = au + csw as f32 / 2.0;
            let sy = av + csh as f32 / 2.0;
            if sx < 0.0 || sy < 0.0 {
                continue;
            }
            let sxi = sx as usize;
            let syi = sy as usize;
            if sxi >= csw || syi >= csh {
                continue;
            }
            let src_idx = ((syi + crop_t) * src_w + (sxi + crop_l)) * 4;
            let dest_idx = dest_row + (dx as usize) * 4;

            let a_src = (src[src_idx + 3] as f32 * opacity) as u8;
            if a_src == 0 {
                continue;
            } else if a_src == 255 {
                base[dest_idx] = src[src_idx];
                base[dest_idx + 1] = src[src_idx + 1];
                base[dest_idx + 2] = src[src_idx + 2];
                base[dest_idx + 3] = 255;
            } else {
                let alpha = a_src as f32 / 255.0;
                let inv_alpha = 1.0 - alpha;
                base[dest_idx] = (src[src_idx] as f32 * alpha + base[dest_idx] as f32 * inv_alpha) as u8;
                base[dest_idx + 1] = (src[src_idx + 1] as f32 * alpha + base[dest_idx + 1] as f32 * inv_alpha) as u8;
                base[dest_idx + 2] = (src[src_idx + 2] as f32 * alpha + base[dest_idx + 2] as f32 * inv_alpha) as u8;
                let a_base = base[dest_idx + 3] as f32 / 255.0;
                base[dest_idx + 3] = ((alpha + a_base * inv_alpha) * 255.0) as u8;
            }
        }
    }
}

fn active_audio_clips_in_range(
    project: &Project,
    start_us: i64,
    end_us: i64,
) -> Vec<(usize, &TrackClip)> {
    let mut result = Vec::new();
    for (track_idx, track) in project.tracks.iter().enumerate() {
        for tc in &track.clips {
            let tc_end = tc.end_us();
            if start_us < tc_end && tc.start_us < end_us {
                result.push((track_idx, tc));
            }
        }
    }
    result
}

// ── Audio Thread Loop ─────────────────────────────────────────────────────

fn audio_loop(
    mut project: Project,
    mut output: AudioOutput,
    ctrl_rx: std_mpsc::Receiver<AudioCtrl>,
) {
    let mut playing = false;
    let mut playhead_us: i64 = 0;
    let mut audio_decoders: HashMap<Uuid, AudioDecoderState> = HashMap::new();
    let mut staging: Vec<f32> = Vec::new();

    const TARGET_BUFFERED: usize = 19_200;
    const BATCH_SAMPLES: usize = 3_840;

    loop {
        let first_ctrl = if playing {
            match ctrl_rx.try_recv() {
                Ok(c) => Some(c),
                Err(std_mpsc::TryRecvError::Empty) => None,
                Err(std_mpsc::TryRecvError::Disconnected) => return,
            }
        } else {
            match ctrl_rx.recv() {
                Ok(c) => Some(c),
                Err(_) => return,
            }
        };

        let mut maybe = first_ctrl;
        while let Some(ctrl) = maybe.take() {
            match ctrl {
                AudioCtrl::Play => playing = true,
                AudioCtrl::Pause => playing = false,
                AudioCtrl::Seek(pts_us) => {
                    let duration_us = project.duration_us();
                    playhead_us = pts_us.clamp(0, duration_us.max(0));
                    staging.clear();
                    // Seek all warm decoders to align with playhead_us
                    for (id, state) in &mut audio_decoders {
                        if let Some((track_idx, clip_idx)) = project.find_clip(*id) {
                            let tc = &project.tracks[track_idx].clips[clip_idx];
                            state.seek_to(playhead_us, tc.start_us, tc.clip.trim_in_us());
                        }
                    }
                }
                AudioCtrl::Stop => return,
                AudioCtrl::SetMuted(m) => output.set_muted(m),
                AudioCtrl::SetVolume(v) => output.set_volume(v),
                AudioCtrl::UpdateProject(proj) => {
                    project = proj;
                    let active_ids: std::collections::HashSet<Uuid> = project.all_clips().map(|c| c.id).collect();
                    audio_decoders.retain(|id, _| active_ids.contains(id));
                }
            }
            match ctrl_rx.try_recv() {
                Ok(c) => maybe = Some(c),
                Err(std_mpsc::TryRecvError::Empty) => break,
                Err(std_mpsc::TryRecvError::Disconnected) => return,
            }
        }

        if !playing {
            continue;
        }

        if !staging.is_empty() {
            let pushed = output.producer.push_slice(&staging);
            staging.drain(..pushed);
        }

        let mut hit_end = false;
        let project_duration_us = project.duration_us();

        while output.producer.vacant_len() > TARGET_BUFFERED {
            let n_frames = BATCH_SAMPLES / 2;
            let sample_duration_us = 1_000_000.0 / 48000.0;
            let duration_us = (n_frames as f64 * sample_duration_us) as i64;
            let t_start_us = playhead_us;
            let t_end_us = t_start_us + duration_us;

            let mut mixed = vec![0.0f32; BATCH_SAMPLES];
            let active = active_audio_clips_in_range(&project, t_start_us, t_end_us);

            for (track_idx, tc) in active {
                let track = &project.tracks[track_idx];
                if track.muted {
                    continue;
                }

                let state = match audio_decoders.get_mut(&tc.clip.id) {
                    Some(s) => s,
                    None => {
                        match FfmpegAudioDecoder::open(&tc.clip.path) {
                            Ok(Some(decoder)) => {
                                audio_decoders.insert(tc.clip.id, AudioDecoderState::new(decoder, t_start_us));
                                audio_decoders.get_mut(&tc.clip.id).unwrap()
                            }
                            _ => continue,
                        }
                    }
                };

                if (state.current_time_us - t_start_us).abs() > 10_000 {
                    state.seek_to(t_start_us, tc.start_us, tc.clip.trim_in_us());
                }

                let samples = state.read_samples(n_frames, tc.start_us, tc.clip.trim_in_us(), tc.clip.trim_out_us());
                let volume = if tc.clip.muted { 0.0 } else { tc.clip.volume };
                for (j, sample) in samples.iter().enumerate() {
                    mixed[j] += sample * volume;
                }
            }

            for sample in &mut mixed {
                *sample = sample.clamp(-1.0, 1.0);
            }

            let pushed = output.producer.push_slice(&mixed);
            if pushed < mixed.len() {
                staging.extend_from_slice(&mixed[pushed..]);
                break;
            }

            playhead_us += duration_us;

            if playhead_us >= project_duration_us {
                hit_end = true;
                break;
            }
        }

        thread::sleep(if hit_end {
            Duration::from_millis(50)
        } else {
            Duration::from_millis(10)
        });
    }
}

// ── Preview metadata ──────────────────────────────────────────────────────

/// Upper bound on the preview's effective frame rate. A display refreshes at
/// ~60Hz and the UI coalesces frames it can't paint in time, so producing more
/// than this many frames per second only burns swscale + GPU-download + memcpy
/// on frames that are discarded before they ever reach the screen. Capping a
/// 120/240fps source here roughly halves (or quarters) the render thread's
/// per-second work with no visible change. Export is unaffected — it renders
/// every source frame at full resolution through a separate path.
const PREVIEW_MAX_FPS: i32 = 60;

/// Native `(width, height, fps_num, fps_den)` of the preview, taken from the
/// first clip in the project (falling back to 1280×720@30 for an empty one).
/// The frame rate is clamped to [`PREVIEW_MAX_FPS`].
fn preview_meta(project: &Project) -> (u32, u32, i32, i32) {
    if let Some(clip) = project.all_clips().next() {
        let den = 1000;
        let mut num = clip.info.profile.frame_rate_mhz as i32;
        // Don't render faster than the display can show; the extra frames are
        // coalesced away by the UI anyway (see the doc on PREVIEW_MAX_FPS).
        if num > PREVIEW_MAX_FPS * den {
            num = PREVIEW_MAX_FPS * den;
        }
        (
            clip.info.profile.resolution.width,
            clip.info.profile.resolution.height,
            num,
            den,
        )
    } else {
        (1280, 720, 30, 1)
    }
}

// ── Render Thread Loop (producer) ─────────────────────────────────────────

/// Decode + composite timeline frames *ahead* of playback and push them into
/// the bounded picture queue. This thread does all the heavy lifting (decode →
/// GPU download → swscale → composite); decoupling it from the present thread
/// is what lets a steady cadence be emitted even when per-frame decode time
/// jitters. Back-pressure from a full queue keeps it only `PICTURE_QUEUE_DEPTH`
/// frames ahead.
fn render_loop(
    mut project: Project,
    ctrl_rx: std_mpsc::Receiver<RenderCtrl>,
    pic_tx: std_mpsc::SyncSender<PictureItem>,
    generation: Arc<AtomicU64>,
) {
    let mut decoders: HashMap<Uuid, DecoderState> = HashMap::new();
    let mut playing = false;
    // Timeline position of the next frame to render.
    let mut render_head_us: i64 = 0;
    // Generation the rendered frames belong to; adopted from each seek/update.
    let mut my_gen: u64 = generation.load(Ordering::Acquire);
    // Render exactly one frame after a seek even while paused, so scrubbing
    // shows the new position without resuming playback.
    let mut force_one = false;
    // A frame that was rendered but couldn't be enqueued (queue full); retried
    // before rendering anything new so we never decode the same frame twice.
    let mut pending_out: Option<PictureItem> = None;

    let (nw, nh, mut fps_num, mut fps_den) = preview_meta(&project);
    let mut preview_scale = preview_scale_for(nw, nh);
    let mut width = scaled_dim(nw, preview_scale);
    let mut height = scaled_dim(nh, preview_scale);
    let mut canvas: Vec<u8> = Vec::new();
    let mut project_duration_us = project.duration_us();
    let mut frame_interval = Duration::from_secs_f32(fps_den as f32 / fps_num as f32);

    let mut compositor = video_merger_engine::composite::gpu::WgpuCompositor::new()
        .map_err(|e| {
            tracing::warn!("GPU Compositor initialization failed, falling back to CPU: {}", e);
            e
        })
        .ok();

    loop {
        // Block for control only when fully idle (paused with nothing buffered
        // to flush and no scrub frame owed); otherwise poll so we stay producing.
        let busy = playing || pending_out.is_some() || force_one;
        let first_ctrl = if busy {
            match ctrl_rx.try_recv() {
                Ok(c) => Some(c),
                Err(std_mpsc::TryRecvError::Empty) => None,
                Err(std_mpsc::TryRecvError::Disconnected) => return,
            }
        } else {
            match ctrl_rx.recv() {
                Ok(c) => Some(c),
                Err(_) => return,
            }
        };

        let mut maybe = first_ctrl;
        while let Some(ctrl) = maybe.take() {
            match ctrl {
                RenderCtrl::Play => playing = true,
                RenderCtrl::Pause => playing = false,
                RenderCtrl::Seek { pts_us, gen } => {
                    my_gen = gen;
                    render_head_us = pts_us.clamp(0, project_duration_us.max(0));
                    // Drop a stale buffered frame and render one at the new spot.
                    pending_out = None;
                    force_one = true;
                }
                RenderCtrl::Stop => return,
                RenderCtrl::UpdateProject { project: proj, gen } => {
                    my_gen = gen;
                    project = proj;
                    project_duration_us = project.duration_us();
                    let (nw, nh, fn_val, fd_val) = preview_meta(&project);
                    preview_scale = preview_scale_for(nw, nh);
                    width = scaled_dim(nw, preview_scale);
                    height = scaled_dim(nh, preview_scale);
                    fps_num = fn_val;
                    fps_den = fd_val;
                    frame_interval = Duration::from_secs_f32(fps_den as f32 / fps_num as f32);
                    pending_out = None;
                    force_one = true;

                    let active_ids: std::collections::HashSet<Uuid> =
                        project.all_clips().map(|c| c.id).collect();
                    decoders.retain(|id, _| active_ids.contains(id));
                }
            }
            match ctrl_rx.try_recv() {
                Ok(c) => maybe = Some(c),
                Err(std_mpsc::TryRecvError::Empty) => break,
                Err(std_mpsc::TryRecvError::Disconnected) => return,
            }
        }

        // Retry a previously-rendered-but-unsent frame first (queue was full).
        if let Some(item) = pending_out.take() {
            match pic_tx.try_send(item) {
                Ok(()) => {
                    if playing {
                        render_head_us += frame_interval.as_micros() as i64;
                    }
                    force_one = false;
                }
                Err(std_mpsc::TrySendError::Full(item)) => {
                    pending_out = Some(item);
                    thread::sleep(Duration::from_millis(2));
                }
                Err(std_mpsc::TrySendError::Disconnected(_)) => return,
            }
            continue;
        }

        // Nothing to do unless playing or a scrub frame is owed.
        if !playing && !force_one {
            continue;
        }

        // Stop producing at end of timeline; the present thread emits the
        // PreviewEnded once its clock crosses the duration.
        if render_head_us >= project_duration_us {
            playing = false;
            force_one = false;
            continue;
        }

        let rgba = render_timeline_frame(
            &project, &mut decoders, preview_scale, &mut canvas,
            compositor.as_mut(),
            render_head_us, width, height,
        );
        let item = PictureItem {
            gen: my_gen,
            frame: DecodedFrame { width, height, pts_us: render_head_us, rgba },
        };
        match pic_tx.try_send(item) {
            Ok(()) => {
                if playing {
                    render_head_us += frame_interval.as_micros() as i64;
                }
                force_one = false;
            }
            Err(std_mpsc::TrySendError::Full(item)) => {
                pending_out = Some(item);
                thread::sleep(Duration::from_millis(2));
            }
            Err(std_mpsc::TrySendError::Disconnected(_)) => return,
        }
    }
}

// ── Present Thread Loop (consumer / pacing) ───────────────────────────────

/// Pace delivery of already-rendered frames to the UI using the audio clock
/// (or wall-clock fallback). This thread never decodes — it only pulls frames
/// from the picture queue, so its cadence is steady regardless of decode jitter.
fn present_loop(
    mut project: Project,
    ctrl_rx: std_mpsc::Receiver<PresentCtrl>,
    pic_rx: std_mpsc::Receiver<PictureItem>,
    render_tx: std_mpsc::Sender<RenderCtrl>,
    event_tx: tokio_mpsc::UnboundedSender<Event>,
    audio_clock: Option<Arc<AtomicU32>>,
    generation: Arc<AtomicU64>,
) {
    let mut playing = false;
    let mut last_delivered_pts_us: i64 = 0;
    let mut clock_anchor: Option<(u32, i64)> = None;
    // Picture generation we currently accept; bumped on seek/update/resync.
    let mut cur_gen: u64 = generation.load(Ordering::Acquire);
    // One look-ahead frame held across ticks (the queue has no peek), so a
    // not-yet-due future frame is never lost.
    let mut pending: Option<PictureItem> = None;
    // Timeline time of the last resync request, to rate-limit them.
    let mut last_resync_us: i64 = i64::MIN;

    let (nw, nh, mut fps_num, mut fps_den) = preview_meta(&project);
    // Only needed for the initial PreviewOpened + black frame; afterwards the
    // present thread forwards queue frames, which carry their own dimensions.
    let width = scaled_dim(nw, preview_scale_for(nw, nh));
    let height = scaled_dim(nh, preview_scale_for(nw, nh));
    let mut project_duration_us = project.duration_us();
    let mut frame_interval = Duration::from_secs_f32(fps_den as f32 / fps_num as f32);
    let mut next_frame_due = Instant::now();

    let _ = event_tx.send(Event::PreviewOpened {
        clip_id: Uuid::nil(),
        duration_us: project_duration_us,
        width,
        height,
        frame_rate_num: fps_num,
        frame_rate_den: fps_den,
    });

    // Initial black frame.
    let mut initial_rgba = vec![0u8; (width * height * 4) as usize];
    for i in (0..initial_rgba.len()).step_by(4) {
        initial_rgba[i + 3] = 255;
    }
    let _ = event_tx.send(Event::FrameReady {
        clip_id: Uuid::nil(),
        frame: DecodedFrame { width, height, pts_us: 0, rgba: initial_rgba.into() },
    });

    loop {
        // When paused we still wake periodically to surface scrub frames the
        // render thread pushes after a seek; when playing we poll without
        // blocking and let the pacing logic below set the cadence.
        let first_ctrl = if playing {
            match ctrl_rx.try_recv() {
                Ok(c) => Some(c),
                Err(std_mpsc::TryRecvError::Empty) => None,
                Err(std_mpsc::TryRecvError::Disconnected) => return,
            }
        } else {
            match ctrl_rx.recv_timeout(Duration::from_millis(16)) {
                Ok(c) => Some(c),
                Err(std_mpsc::RecvTimeoutError::Timeout) => None,
                Err(std_mpsc::RecvTimeoutError::Disconnected) => return,
            }
        };

        let mut maybe = first_ctrl;
        while let Some(ctrl) = maybe.take() {
            match ctrl {
                PresentCtrl::Play => {
                    playing = true;
                    next_frame_due = Instant::now();
                    clock_anchor = None;
                }
                PresentCtrl::Pause => {
                    playing = false;
                    clock_anchor = None;
                }
                PresentCtrl::Seek { pts_us, gen } => {
                    cur_gen = gen;
                    last_delivered_pts_us = pts_us.clamp(0, project_duration_us.max(0));
                    clock_anchor = None;
                    next_frame_due = Instant::now();
                    last_resync_us = i64::MIN;
                    pending = None; // drop frames from the old position
                }
                PresentCtrl::Stop => {
                    let _ = event_tx.send(Event::PreviewEnded { clip_id: Uuid::nil() });
                    return;
                }
                PresentCtrl::UpdateProject { project: proj, gen } => {
                    cur_gen = gen;
                    project = proj;
                    project_duration_us = project.duration_us();
                    let (_, _, fn_val, fd_val) = preview_meta(&project);
                    fps_num = fn_val;
                    fps_den = fd_val;
                    frame_interval = Duration::from_secs_f32(fps_den as f32 / fps_num as f32);
                    pending = None;
                }
            }
            match ctrl_rx.try_recv() {
                Ok(c) => maybe = Some(c),
                Err(std_mpsc::TryRecvError::Empty) => break,
                Err(std_mpsc::TryRecvError::Disconnected) => return,
            }
        }

        if !playing {
            // Paused: surface the latest valid frame (e.g. a scrub result),
            // ignoring pacing. `take_frame` with `None` returns the newest.
            if let Some(frame) = take_frame(&pic_rx, cur_gen, &mut pending, None) {
                last_delivered_pts_us = frame.pts_us;
                if event_tx.send(Event::FrameReady { clip_id: Uuid::nil(), frame }).is_err() {
                    return;
                }
            }
            continue;
        }

        if let Some(clock) = audio_clock.as_ref() {
            let anchor = match clock_anchor {
                Some(a) => a,
                None => {
                    let a = (clock.load(Ordering::Relaxed), last_delivered_pts_us);
                    clock_anchor = Some(a);
                    a
                }
            };
            let frames_now = clock.load(Ordering::Relaxed);
            let elapsed_us = (frames_now.wrapping_sub(anchor.0) as i64) * 1_000_000
                / video_merger_engine::audio::SAMPLE_RATE as i64;
            let target_local_us = anchor.1 + elapsed_us;

            if target_local_us >= project_duration_us {
                let _ = event_tx.send(Event::PreviewEnded { clip_id: Uuid::nil() });
                playing = false;
                clock_anchor = None;
                continue;
            }

            // Deliver the newest buffered frame whose pts has come due.
            if let Some(frame) = take_frame(&pic_rx, cur_gen, &mut pending, Some(target_local_us)) {
                last_delivered_pts_us = frame.pts_us;
                if event_tx.send(Event::FrameReady { clip_id: Uuid::nil(), frame }).is_err() {
                    return;
                }
            }

            // If video has fallen far behind the audio clock (decode can't keep
            // up), ask the render thread to jump forward to the clock so we
            // resync instead of grinding through frames that are already late.
            if target_local_us - last_delivered_pts_us > 250_000
                && target_local_us - last_resync_us > 250_000
            {
                let gen = generation.fetch_add(1, Ordering::AcqRel) + 1;
                cur_gen = gen;
                pending = None;
                last_resync_us = target_local_us;
                let _ = render_tx.send(RenderCtrl::Seek { pts_us: target_local_us, gen });
            }

            // Sleep until roughly the next frame boundary to avoid busy-waiting.
            thread::sleep(Duration::from_millis(2));
            continue;
        }

        // Fallback: wall-clock pacing (no audio output).
        let now = Instant::now();
        if now < next_frame_due {
            thread::sleep(next_frame_due - now);
        }
        next_frame_due += frame_interval;

        let target_local_us = last_delivered_pts_us + frame_interval.as_micros() as i64;
        if target_local_us >= project_duration_us {
            let _ = event_tx.send(Event::PreviewEnded { clip_id: Uuid::nil() });
            playing = false;
            continue;
        }

        if let Some(frame) = take_frame(&pic_rx, cur_gen, &mut pending, Some(target_local_us)) {
            last_delivered_pts_us = frame.pts_us;
            if event_tx.send(Event::FrameReady { clip_id: Uuid::nil(), frame }).is_err() {
                return;
            }
        }
    }
}

/// Pull frames from the picture queue, dropping any whose generation no longer
/// matches `cur_gen`, and return the newest one that is *due*:
///   * `target = Some(t)` → the latest frame with `pts_us <= t` (a not-yet-due
///     frame is kept in `pending` for a later tick);
///   * `target = None` → the newest available valid frame (used while paused).
/// Returns `None` when nothing valid/due is buffered.
fn take_frame(
    pic_rx: &std_mpsc::Receiver<PictureItem>,
    cur_gen: u64,
    pending: &mut Option<PictureItem>,
    target: Option<i64>,
) -> Option<DecodedFrame> {
    let mut chosen: Option<DecodedFrame> = None;
    loop {
        if pending.is_none() {
            match pic_rx.try_recv() {
                Ok(item) if item.gen == cur_gen => *pending = Some(item),
                Ok(_) => continue, // stale generation → drop and keep draining
                Err(_) => break,
            }
        }
        let due = match target {
            Some(t) => pending.as_ref().unwrap().frame.pts_us <= t,
            None => true,
        };
        if due {
            chosen = Some(pending.take().unwrap().frame);
        } else {
            break;
        }
    }
    chosen
}

/// Get a warm decoder for `tc`, opening (and caching) one on first use.
/// Returns `None` only when the file cannot be opened.
fn decoder_for<'a>(
    decoders: &'a mut HashMap<Uuid, DecoderState>,
    tc: &TrackClip,
    preview_scale: f32,
) -> Option<&'a mut DecoderState> {
    if !decoders.contains_key(&tc.clip.id) {
        match FfmpegMediaDecoder::open_scaled(&tc.clip.path, preview_scale) {
            Ok(dec) => {
                decoders.insert(tc.clip.id, DecoderState::new(dec));
            }
            Err(e) => {
                tracing::warn!("Failed to open video decoder for clip {}: {}", tc.clip.id, e);
                return None;
            }
        }
    }
    decoders.get_mut(&tc.clip.id)
}

/// Render the composited timeline frame at `t_us`.
///
/// Returns the RGBA buffer as an `Arc<[u8]>`. `canvas` is a scratch buffer
/// reused across frames so the compositing path allocates nothing per frame.
/// The fast path (a single opaque, untransformed, full-canvas clip — the common
/// single-clip preview case) bypasses the canvas entirely and forwards the
/// decoder's own RGBA buffer with zero copy.
fn render_timeline_frame(
    project: &Project,
    decoders: &mut HashMap<Uuid, DecoderState>,
    preview_scale: f32,
    canvas: &mut Vec<u8>,
    compositor: Option<&mut video_merger_engine::composite::gpu::WgpuCompositor>,
    t_us: i64,
    width: u32,
    height: u32,
) -> Arc<[u8]> {
    let mut active = video_merger_engine::composite::active_clips_at(project, t_us);
    // Higher video tracks (V2) composite on top of lower ones (V1).
    active.sort_by_key(|(track_idx, _)| match project.tracks[*track_idx].name.as_str() {
        "V2" => 1,
        _ => 0,
    });

    // Fast path: one identity-transform clip that fills the canvas → forward
    // the decoder's RGBA Arc directly (no canvas fill, no sampling, no composite).
    // `rel_us` (since the clip's visible start) drives the transform/keyframes;
    // `src_us` (with trim_in) drives the decoder seek.
    if let [(_, tc)] = active.as_slice() {
        let rel_us = t_us - tc.start_us;
        let src_us = rel_us + tc.clip.trim_in_us();
        if tc.clip.resolved_transform(rel_us).is_identity() {
            if let Some(state) = decoder_for(decoders, tc, preview_scale) {
                if let Some(frame) = state.get_frame_at(src_us) {
                    if frame.width == width && frame.height == height {
                        return frame.rgba;
                    }
                }
            }
        }
    }

    // Slow path: composite every active clip onto the reused canvas.
    let needed = (width as usize) * (height as usize) * 4;
    if canvas.len() != needed {
        canvas.resize(needed, 0);
    }

    // Collect all successfully decoded frames and their transforms
    let mut decoded_frames = Vec::new();
    for (_, tc) in active {
        let rel_us = t_us - tc.start_us;
        let src_us = rel_us + tc.clip.trim_in_us();
        let xf = tc.clip.resolved_transform(rel_us);

        if let Some(state) = decoder_for(decoders, tc, preview_scale) {
            if let Some(frame) = state.get_frame_at(src_us) {
                decoded_frames.push((frame, xf));
            }
        }
    }

    // Attempt GPU compositing if supported
    if let Some(comp) = compositor {
        let mut gpu_inputs = Vec::new();
        for (frame, xf) in &decoded_frames {
            let (crop_l, crop_t, crop_r, crop_b) = match xf.crop {
                Some(c) => (
                    (c.left as f32 * preview_scale).round() as usize,
                    (c.top as f32 * preview_scale).round() as usize,
                    (c.right as f32 * preview_scale).round() as usize,
                    (c.bottom as f32 * preview_scale).round() as usize,
                ),
                None => (0, 0, 0, 0),
            };

            let center_x = width as f32 / 2.0 + xf.position_x as f32 * preview_scale;
            let center_y = height as f32 / 2.0 + xf.position_y as f32 * preview_scale;

            gpu_inputs.push(video_merger_engine::composite::gpu::GpuClipInput {
                rgba: &frame.rgba,
                width: frame.width as usize,
                height: frame.height as usize,
                crop_l,
                crop_t,
                crop_r,
                crop_b,
                scale_x: xf.scale_x,
                scale_y: xf.scale_y,
                rotation_deg: xf.rotation_deg,
                flip_h: xf.flip_h,
                flip_v: xf.flip_v,
                opacity: xf.opacity,
                center_x,
                center_y,
                brightness: xf.brightness,
                contrast: xf.contrast,
                saturation: xf.saturation,
            });
        }

        match comp.composite(width as usize, height as usize, canvas.as_mut_slice(), &gpu_inputs) {
            Ok(()) => {
                return Arc::from(canvas.as_slice());
            }
            Err(e) => {
                tracing::error!("GPU Compositing failed: {}. Falling back to CPU.", e);
            }
        }
    }

    // CPU Fallback path
    // Reset to opaque black (R=G=B=0, A=255).
    canvas.fill(0);
    for i in (3..canvas.len()).step_by(4) {
        canvas[i] = 255;
    }

    for (frame, xf) in &decoded_frames {
        let (crop_l, crop_t, crop_r, crop_b) = match xf.crop {
            Some(c) => (
                (c.left as f32 * preview_scale).round() as usize,
                (c.top as f32 * preview_scale).round() as usize,
                (c.right as f32 * preview_scale).round() as usize,
                (c.bottom as f32 * preview_scale).round() as usize,
            ),
            None => (0, 0, 0, 0),
        };

        let center_x = width as f32 / 2.0 + xf.position_x as f32 * preview_scale;
        let center_y = height as f32 / 2.0 + xf.position_y as f32 * preview_scale;

        composite_affine(
            canvas.as_mut_slice(), width as usize, height as usize,
            &frame.rgba, frame.width as usize, frame.height as usize,
            crop_l, crop_t, crop_r, crop_b,
            xf.scale_x, xf.scale_y, xf.rotation_deg, xf.flip_h, xf.flip_v,
            xf.opacity, center_x, center_y,
        );
    }

    Arc::from(canvas.as_slice())
}

// ── Performance harness ────────────────────────────────────────────────────
//
// These are micro-benchmarks, not correctness tests: they print per-frame
// timings so we can tell whether the affine compositor and/or 4K decode keep up
// with the frame budget. Run in RELEASE (debug numbers are meaningless):
//
//   cargo test --release -p video-merger-worker perf -- --nocapture --test-threads=1
//
// `perf_compositor` is self-contained; `perf_render_e2e` needs the sample clip
// at /tmp/cutoff_test_10s.mp4 (and is skipped otherwise).
#[cfg(test)]
mod perf {
    use super::*;
    use std::path::PathBuf;
    use std::time::Duration as StdDuration;
    use video_merger_core::domain::{
        Clip, CodecProfile, MediaInfo, Project, Resolution, TrackClip, TrackKind,
    };

    const BUDGET_30: f64 = 1000.0 / 30.0; // 33.3 ms
    const BUDGET_60: f64 = 1000.0 / 60.0; // 16.7 ms

    /// Time `iters` runs of `f`, return (avg_ms, min_ms, max_ms, p95_ms).
    fn bench(iters: usize, mut f: impl FnMut(usize)) -> (f64, f64, f64, f64) {
        let mut samples = Vec::with_capacity(iters);
        for i in 0..iters {
            let t = Instant::now();
            f(i);
            samples.push(t.elapsed().as_secs_f64() * 1000.0);
        }
        samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let sum: f64 = samples.iter().sum();
        let p95_idx = ((iters as f64 * 0.95) as usize).min(iters - 1);
        (sum / iters as f64, samples[0], samples[iters - 1], samples[p95_idx])
    }

    fn report(label: &str, (avg, min, max, p95): (f64, f64, f64, f64)) {
        let verdict30 = if p95 <= BUDGET_30 { "OK@30" } else { "MISS@30" };
        let verdict60 = if p95 <= BUDGET_60 { "OK@60" } else { "miss@60" };
        println!(
            "  {label:<34} avg={avg:6.2}ms  p95={p95:6.2}ms  (min={min:5.2} max={max:6.2})  [{verdict30} {verdict60}]"
        );
    }

    #[test]
    fn perf_compositor() {
        // Preview-sized canvas + source (decoder already downscales to ≤1280).
        let (w, h) = (1280usize, 720usize);
        let mut base = vec![0u8; w * h * 4];
        let mut src = vec![0u8; w * h * 4];
        for i in (0..src.len()).step_by(4) {
            src[i] = 200; src[i + 1] = 100; src[i + 2] = 50; src[i + 3] = 255;
        }
        let cx = w as f32 / 2.0;
        let cy = h as f32 / 2.0;
        let iters = 400;

        println!("\ncomposite_affine — {w}x{h} canvas, {iters} iters (budget: 33.3ms@30, 16.7ms@60)");
        report("scale 1.0 (no rotation)", bench(iters, |_| {
            composite_affine(&mut base, w, h, &src, w, h, 0,0,0,0, 1.0,1.0, 0.0, false,false, 1.0, cx, cy);
        }));
        report("scale 1.5", bench(iters, |_| {
            composite_affine(&mut base, w, h, &src, w, h, 0,0,0,0, 1.5,1.5, 0.0, false,false, 1.0, cx, cy);
        }));
        report("opacity 0.5 (alpha blend)", bench(iters, |_| {
            composite_affine(&mut base, w, h, &src, w, h, 0,0,0,0, 1.0,1.0, 0.0, false,false, 0.5, cx, cy);
        }));
        report("rotation 30deg (trig path)", bench(iters, |_| {
            composite_affine(&mut base, w, h, &src, w, h, 0,0,0,0, 1.0,1.0, 30.0, false,false, 1.0, cx, cy);
        }));
        report("rotation 30 + scale 1.5", bench(iters, |_| {
            composite_affine(&mut base, w, h, &src, w, h, 0,0,0,0, 1.5,1.5, 30.0, false,false, 1.0, cx, cy);
        }));
    }

    fn sample_project(rotation: f32, scale: f32) -> Option<Project> {
        let path = PathBuf::from("/tmp/cutoff_test_10s.mp4");
        if !path.exists() {
            eprintln!("skipping perf_render_e2e: {} not present", path.display());
            return None;
        }
        let mut clip = Clip::new(
            path,
            MediaInfo {
                duration: StdDuration::from_secs(10),
                profile: CodecProfile {
                    video_codec: "h264".into(),
                    audio_codec: "aac".into(),
                    resolution: Resolution { width: 3840, height: 2160 },
                    frame_rate_mhz: 30_000,
                    pixel_format: "yuv420p".into(),
                },
            },
        );
        clip.rotation_deg = rotation;
        clip.scale = scale;
        let mut proj = Project::with_default_tracks();
        let v1 = proj.track_index_by_name("V1").unwrap_or(1);
        proj.tracks[v1].clips.push(TrackClip { clip, start_us: 0 });
        let _ = TrackKind::Video;
        Some(proj)
    }

    #[test]
    fn perf_render_e2e() {
        let frames = 240; // 8s @ 30fps of sequential playback
        let scenarios: [(&str, f32, f32); 3] = [
            ("untransformed (fast path)", 0.0, 1.0),
            ("scale 1.5 (slow path)", 0.0, 1.5),
            ("rotation 30 (slow path)", 30.0, 1.0),
        ];
        println!("\nrender_timeline_frame end-to-end — real 4K decode, {frames} frames (budget 33.3ms@30)");
        for (label, rot, scale) in scenarios {
            let Some(project) = sample_project(rot, scale) else { return };
            let (nw, nh, _, _) = preview_meta(&project);
            let ps = preview_scale_for(nw, nh);
            let (width, height) = (scaled_dim(nw, ps), scaled_dim(nh, ps));
            let mut decoders: HashMap<Uuid, DecoderState> = HashMap::new();
            let mut canvas: Vec<u8> = Vec::new();
            let step = 1_000_000i64 / 30;
            // Warm up the decoder (first frame pays open + seek cost).
            let _ = render_timeline_frame(&project, &mut decoders, ps, &mut canvas, None, 0, width, height);
            let stats = bench(frames, |i| {
                let t = step * i as i64;
                let _ = render_timeline_frame(&project, &mut decoders, ps, &mut canvas, None, t, width, height);
            });
            report(label, stats);
        }
    }
}
