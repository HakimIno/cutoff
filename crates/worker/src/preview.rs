//! Background preview-playback threads.
//!
//! Each preview session owns:
//!   * a **video thread** that decodes frames and paces delivery to the UI
//!     using the audio clock (or wall-clock fallback);
//!   * an **audio thread** (optional) that
//!     owns the `!Send` cpal `Stream` and feeds the ring buffer continuously.
//!
//! Coordination is by `std::sync::mpsc` control channels + the shared
//! `Arc<AtomicU32>` audio clock from `AudioOutput`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
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

// ── Internal per-thread message types ─────────────────────────────────────

enum VideoCtrl {
    Play,
    Pause,
    Seek(i64),
    Stop,
    UpdateProject(Project),
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
    video_tx: std_mpsc::Sender<VideoCtrl>,
    audio_tx: Option<std_mpsc::Sender<AudioCtrl>>,
    _video_join: thread::JoinHandle<()>,
    _audio_join: Option<thread::JoinHandle<()>>,
}

impl PreviewHandle {
    pub fn send(&self, ctrl: PreviewCtrl) {
        match ctrl {
            PreviewCtrl::Play => {
                let _ = self.video_tx.send(VideoCtrl::Play);
                if let Some(tx) = &self.audio_tx {
                    let _ = tx.send(AudioCtrl::Play);
                }
            }
            PreviewCtrl::Pause => {
                let _ = self.video_tx.send(VideoCtrl::Pause);
                if let Some(tx) = &self.audio_tx {
                    let _ = tx.send(AudioCtrl::Pause);
                }
            }
            PreviewCtrl::Seek { pts_us } => {
                let _ = self.video_tx.send(VideoCtrl::Seek(pts_us));
                if let Some(tx) = &self.audio_tx {
                    let _ = tx.send(AudioCtrl::Seek(pts_us));
                }
            }
            PreviewCtrl::Stop => {
                let _ = self.video_tx.send(VideoCtrl::Stop);
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
                let _ = self.video_tx.send(VideoCtrl::UpdateProject(project.clone()));
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

    let (video_tx, video_rx) = std_mpsc::channel::<VideoCtrl>();
    let proj_v = project;
    let video_join = thread::Builder::new()
        .name("cutoff-preview-video".into())
        .spawn(move || {
            video_loop(proj_v, video_rx, event_tx, audio_clock);
        })
        .expect("video preview thread spawn");

    PreviewHandle {
        video_tx,
        audio_tx,
        _video_join: video_join,
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

// ── Alpha Compositing & Resizing ──────────────────────────────────────────

fn resize_rgba(src: &[u8], src_w: usize, src_h: usize, dest: &mut [u8], dest_w: usize, dest_h: usize) {
    for dy in 0..dest_h {
        let sy = (dy * src_h) / dest_h;
        let src_row_offset = sy * src_w * 4;
        let dest_row_offset = dy * dest_w * 4;
        for dx in 0..dest_w {
            let sx = (dx * src_w) / dest_w;
            let src_idx = src_row_offset + sx * 4;
            let dest_idx = dest_row_offset + dx * 4;
            dest[dest_idx..dest_idx + 4].copy_from_slice(&src[src_idx..src_idx + 4]);
        }
    }
}

fn composite_rgba_transformed(
    base: &mut [u8],
    base_w: usize,
    base_h: usize,
    overlay: &[u8],
    overlay_w: usize,
    overlay_h: usize,
    offset_x: i32,
    offset_y: i32,
    opacity: f32,
) {
    if opacity <= 0.0 {
        return;
    }
    
    for dy in 0..overlay_h {
        let dest_y = offset_y + dy as i32;
        if dest_y < 0 || dest_y >= base_h as i32 {
            continue;
        }
        let src_row_offset = dy * overlay_w * 4;
        let dest_row_offset = (dest_y as usize) * base_w * 4;
        
        for dx in 0..overlay_w {
            let dest_x = offset_x + dx as i32;
            if dest_x < 0 || dest_x >= base_w as i32 {
                continue;
            }
            let src_idx = src_row_offset + dx * 4;
            let dest_idx = dest_row_offset + (dest_x as usize) * 4;
            
            let a_overlay = (overlay[src_idx + 3] as f32 * opacity) as u8;
            if a_overlay == 0 {
                continue;
            } else if a_overlay == 255 {
                base[dest_idx] = overlay[src_idx];
                base[dest_idx + 1] = overlay[src_idx + 1];
                base[dest_idx + 2] = overlay[src_idx + 2];
                base[dest_idx + 3] = a_overlay;
            } else {
                let alpha = a_overlay as f32 / 255.0;
                let inv_alpha = 1.0 - alpha;
                base[dest_idx] = (overlay[src_idx] as f32 * alpha + base[dest_idx] as f32 * inv_alpha) as u8;
                base[dest_idx + 1] = (overlay[src_idx + 1] as f32 * alpha + base[dest_idx + 1] as f32 * inv_alpha) as u8;
                base[dest_idx + 2] = (overlay[src_idx + 2] as f32 * alpha + base[dest_idx + 2] as f32 * inv_alpha) as u8;
                let a_base = base[dest_idx + 3] as f32 / 255.0;
                let combined_a = alpha + a_base * inv_alpha;
                base[dest_idx + 3] = (combined_a * 255.0) as u8;
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

// ── Video Thread Loop ─────────────────────────────────────────────────────

fn video_loop(
    mut project: Project,
    ctrl_rx: std_mpsc::Receiver<VideoCtrl>,
    event_tx: tokio_mpsc::UnboundedSender<Event>,
    audio_clock: Option<Arc<AtomicU32>>,
) {
    let mut decoders: HashMap<Uuid, DecoderState> = HashMap::new();
    let mut playing = false;
    let mut last_delivered_pts_us: i64 = 0;
    let mut clock_anchor: Option<(u32, i64)> = None;

    let get_preview_meta = |proj: &Project| {
        if let Some(clip) = proj.all_clips().next() {
            (
                clip.info.profile.resolution.width,
                clip.info.profile.resolution.height,
                clip.info.profile.frame_rate_mhz as i32,
                1000,
            )
        } else {
            (1280, 720, 30, 1)
        }
    };

    let (native_w, native_h, mut fps_num, mut fps_den) = get_preview_meta(&project);
    let mut preview_scale = preview_scale_for(native_w, native_h);
    let mut width = scaled_dim(native_w, preview_scale);
    let mut height = scaled_dim(native_h, preview_scale);
    // Scratch buffer reused by the compositing path so it allocates nothing
    // per frame; grown on demand inside `render_timeline_frame`.
    let mut canvas: Vec<u8> = Vec::new();
    let mut project_duration_us = project.duration_us();
    let mut frame_interval = Duration::from_secs_f32(fps_den as f32 / fps_num as f32);
    let mut next_frame_due = Instant::now();

    // Send initial PreviewOpened event
    let _ = event_tx.send(Event::PreviewOpened {
        clip_id: Uuid::nil(),
        duration_us: project_duration_us,
        width,
        height,
        frame_rate_num: fps_num,
        frame_rate_den: fps_den,
    });

    // Send initial black frame
    let mut initial_rgba = vec![0u8; (width * height * 4) as usize];
    for i in (0..initial_rgba.len()).step_by(4) {
        initial_rgba[i + 3] = 255;
    }
    let frame = DecodedFrame {
        width,
        height,
        pts_us: 0,
        rgba: initial_rgba.into(),
    };
    let _ = event_tx.send(Event::FrameReady {
        clip_id: Uuid::nil(),
        frame,
    });

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
                VideoCtrl::Play => {
                    playing = true;
                    next_frame_due = Instant::now();
                    clock_anchor = None;
                }
                VideoCtrl::Pause => {
                    playing = false;
                    clock_anchor = None;
                }
                VideoCtrl::Seek(pts_us) => {
                    last_delivered_pts_us = pts_us.clamp(0, project_duration_us.max(0));
                    clock_anchor = None;
                    next_frame_due = Instant::now();

                    // Render seeker frame immediately
                    let rgba = render_timeline_frame(
                        &project, &mut decoders, preview_scale, &mut canvas,
                        last_delivered_pts_us, width, height,
                    );
                    let frame = DecodedFrame {
                        width,
                        height,
                        pts_us: last_delivered_pts_us,
                        rgba,
                    };
                    let _ = event_tx.send(Event::FrameReady {
                        clip_id: Uuid::nil(),
                        frame,
                    });
                }
                VideoCtrl::Stop => {
                    let _ = event_tx.send(Event::PreviewEnded { clip_id: Uuid::nil() });
                    return;
                }
                VideoCtrl::UpdateProject(proj) => {
                    project = proj;
                    project_duration_us = project.duration_us();
                    let (nw, nh, fn_val, fd_val) = get_preview_meta(&project);
                    preview_scale = preview_scale_for(nw, nh);
                    width = scaled_dim(nw, preview_scale);
                    height = scaled_dim(nh, preview_scale);
                    fps_num = fn_val;
                    fps_den = fd_val;
                    frame_interval = Duration::from_secs_f32(fps_den as f32 / fps_num as f32);

                    let active_ids: std::collections::HashSet<Uuid> = project.all_clips().map(|c| c.id).collect();
                    decoders.retain(|id, _| active_ids.contains(id));
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

            if target_local_us - last_delivered_pts_us > 200_000 {
                // Audio is far ahead (or we seeked), skip to target_local_us
                last_delivered_pts_us = target_local_us;
            }

            let frame_duration_us = frame_interval.as_micros() as i64;
            let time_since_last_frame = target_local_us - last_delivered_pts_us;
            if time_since_last_frame < frame_duration_us {
                let wait_us = frame_duration_us - time_since_last_frame;
                thread::sleep(Duration::from_micros((wait_us.min(10_000)) as u64));
                continue;
            }

            // Render and send frame
            let rgba = render_timeline_frame(
                &project, &mut decoders, preview_scale, &mut canvas,
                target_local_us, width, height,
            );
            let frame = DecodedFrame {
                width,
                height,
                pts_us: target_local_us,
                rgba,
            };
            last_delivered_pts_us = target_local_us;
            if event_tx.send(Event::FrameReady { clip_id: Uuid::nil(), frame }).is_err() {
                return;
            }
            continue;
        }

        // Fallback: Wall-clock pacing
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

        let rgba = render_timeline_frame(
            &project, &mut decoders, preview_scale, &mut canvas,
            target_local_us, width, height,
        );
        let frame = DecodedFrame {
            width,
            height,
            pts_us: target_local_us,
            rgba,
        };
        last_delivered_pts_us = target_local_us;
        if event_tx.send(Event::FrameReady { clip_id: Uuid::nil(), frame }).is_err() {
            return;
        }
    }
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

    // Fast path: one opaque, untransformed clip that fills the canvas → forward
    // the decoder's RGBA Arc directly (no canvas fill, no resize, no composite).
    if let [(_, tc)] = active.as_slice() {
        let untransformed = tc.clip.opacity >= 1.0
            && (tc.clip.scale - 1.0).abs() < 1e-3
            && tc.clip.position_x == 0
            && tc.clip.position_y == 0;
        if untransformed {
            if let Some(state) = decoder_for(decoders, tc, preview_scale) {
                let target_src_us = t_us - tc.start_us + tc.clip.trim_in_us();
                if let Some(frame) = state.get_frame_at(target_src_us) {
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
    // Reset to opaque black (R=G=B=0, A=255).
    canvas.fill(0);
    for i in (3..canvas.len()).step_by(4) {
        canvas[i] = 255;
    }

    for (_, tc) in active {
        let target_src_us = t_us - tc.start_us + tc.clip.trim_in_us();
        let (scale, opacity, pos_x, pos_y) =
            (tc.clip.scale, tc.clip.opacity, tc.clip.position_x, tc.clip.position_y);

        let frame = match decoder_for(decoders, tc, preview_scale) {
            Some(state) => match state.get_frame_at(target_src_us) {
                Some(f) => f,
                None => continue,
            },
            None => continue,
        };

        let overlay_w = ((frame.width as f32 * scale).round() as usize).max(1);
        let overlay_h = ((frame.height as f32 * scale).round() as usize).max(1);

        let center_offset_x = (width as i32 - overlay_w as i32) / 2;
        let center_offset_y = (height as i32 - overlay_h as i32) / 2;
        // Clip positions are authored in full project-resolution space, so
        // scale them to match the (possibly downscaled) preview canvas.
        let offset_x = (pos_x as f32 * preview_scale).round() as i32 + center_offset_x;
        let offset_y = (pos_y as f32 * preview_scale).round() as i32 + center_offset_y;

        if overlay_w != frame.width as usize || overlay_h != frame.height as usize {
            let mut resized = vec![0u8; overlay_w * overlay_h * 4];
            resize_rgba(&frame.rgba, frame.width as usize, frame.height as usize, &mut resized, overlay_w, overlay_h);
            composite_rgba_transformed(
                canvas.as_mut_slice(), width as usize, height as usize,
                &resized, overlay_w, overlay_h, offset_x, offset_y, opacity,
            );
        } else {
            composite_rgba_transformed(
                canvas.as_mut_slice(), width as usize, height as usize,
                &frame.rgba, frame.width as usize, frame.height as usize, offset_x, offset_y, opacity,
            );
        }
    }

    Arc::from(canvas.as_slice())
}
