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
            Some(last) => {
                target_us < last || target_us - last > threshold_us
            }
        };
        
        if needs_seek {
            if let Err(e) = self.decoder.seek_to_us(target_us) {
                tracing::warn!("Seek failed: {}", e);
                return None;
            }
            self.last_pts_us = None;
        }
        
        let mut best_frame: Option<DecodedFrame> = None;
        loop {
            match self.decoder.next_frame() {
                Ok(Some(frame)) => {
                    self.last_pts_us = Some(frame.pts_us);
                    if frame.pts_us >= target_us {
                        if let Some(prev) = best_frame {
                            if (target_us - prev.pts_us).abs() < (frame.pts_us - target_us).abs() {
                                return Some(prev);
                            }
                        }
                        return Some(frame);
                    }
                    best_frame = Some(frame);
                }
                Ok(None) => break,
                Err(e) => {
                    tracing::warn!("Decode frame failed: {}", e);
                    break;
                }
            }
        }
        best_frame
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

fn composite_rgba(base: &mut [u8], overlay: &[u8]) {
    assert_eq!(base.len(), overlay.len());
    for i in (0..base.len()).step_by(4) {
        let a_overlay = overlay[i + 3];
        if a_overlay == 0 {
            continue;
        } else if a_overlay == 255 {
            base[i] = overlay[i];
            base[i + 1] = overlay[i + 1];
            base[i + 2] = overlay[i + 2];
            base[i + 3] = overlay[i + 3];
        } else {
            let alpha = a_overlay as f32 / 255.0;
            let inv_alpha = 1.0 - alpha;
            base[i] = (overlay[i] as f32 * alpha + base[i] as f32 * inv_alpha) as u8;
            base[i + 1] = (overlay[i + 1] as f32 * alpha + base[i + 1] as f32 * inv_alpha) as u8;
            base[i + 2] = (overlay[i + 2] as f32 * alpha + base[i + 2] as f32 * inv_alpha) as u8;
            let a_base = base[i + 3] as f32 / 255.0;
            let combined_a = alpha + a_base * inv_alpha;
            base[i + 3] = (combined_a * 255.0) as u8;
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

    let (mut width, mut height, mut fps_num, mut fps_den) = get_preview_meta(&project);
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
                    let mut session = VideoSession { project: project.clone(), decoders };
                    let rgba = render_timeline_frame(&mut session, last_delivered_pts_us, width, height);
                    decoders = session.decoders;
                    let frame = DecodedFrame {
                        width,
                        height,
                        pts_us: last_delivered_pts_us,
                        rgba: rgba.into(),
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
                    let (w, h, fn_val, fd_val) = get_preview_meta(&project);
                    width = w;
                    height = h;
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
            let mut session = VideoSession { project: project.clone(), decoders };
            let rgba = render_timeline_frame(&mut session, target_local_us, width, height);
            decoders = session.decoders;
            let frame = DecodedFrame {
                width,
                height,
                pts_us: target_local_us,
                rgba: rgba.into(),
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

        let mut session = VideoSession { project: project.clone(), decoders };
        let rgba = render_timeline_frame(&mut session, target_local_us, width, height);
        decoders = session.decoders;
        let frame = DecodedFrame {
            width,
            height,
            pts_us: target_local_us,
            rgba: rgba.into(),
        };
        last_delivered_pts_us = target_local_us;
        if event_tx.send(Event::FrameReady { clip_id: Uuid::nil(), frame }).is_err() {
            return;
        }
    }
}

struct VideoSession {
    project: Project,
    decoders: HashMap<Uuid, DecoderState>,
}

fn render_timeline_frame(
    session: &mut VideoSession,
    t_us: i64,
    width: u32,
    height: u32,
) -> Vec<u8> {
    let mut canvas = vec![0u8; (width * height * 4) as usize];
    for i in (0..canvas.len()).step_by(4) {
        canvas[i + 3] = 255;
    }
    let mut active = video_merger_engine::composite::active_clips_at(&session.project, t_us);
    
    active.sort_by_key(|(track_idx, _)| {
        let name = &session.project.tracks[*track_idx].name;
        match name.as_str() {
            "V1" => 0,
            "V2" => 1,
            _ => 0,
        }
    });

    for (track_idx, tc) in active {
        let track = &session.project.tracks[track_idx];
        if track.muted {
            continue;
        }
        
        let state = match session.decoders.get_mut(&tc.clip.id) {
            Some(s) => s,
            None => {
                match FfmpegMediaDecoder::open(&tc.clip.path) {
                    Ok(dec) => {
                        session.decoders.insert(tc.clip.id, DecoderState::new(dec));
                        session.decoders.get_mut(&tc.clip.id).unwrap()
                    }
                    Err(e) => {
                        tracing::warn!("Failed to open video decoder for clip {}: {}", tc.clip.id, e);
                        continue;
                    }
                }
            }
        };
        
        let target_src_us = t_us - tc.start_us + tc.clip.trim_in_us();
        if let Some(frame) = state.get_frame_at(target_src_us) {
            if frame.width != width || frame.height != height {
                let mut resized = vec![0u8; (width * height * 4) as usize];
                resize_rgba(&frame.rgba, frame.width as usize, frame.height as usize, &mut resized, width as usize, height as usize);
                composite_rgba(&mut canvas, &resized);
            } else {
                composite_rgba(&mut canvas, &frame.rgba);
            }
        }
    }
    
    canvas
}
