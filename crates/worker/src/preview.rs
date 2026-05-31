//! Background preview-playback threads.
//!
//! Each preview session owns:
//!   * a **video thread** that decodes frames and paces delivery to the UI
//!     using the audio clock (or wall-clock fallback);
//!   * an **audio thread** (optional, only when the source has audio) that
//!     owns the `!Send` cpal `Stream` and feeds the ring buffer continuously.
//!
//! The two threads run in true parallel: ffmpeg video decode (which is itself
//! multi-threaded internally) never blocks audio decode and vice versa.
//! Coordination is by `std::sync::mpsc` control channels + the shared
//! `Arc<AtomicU32>` audio clock from `AudioOutput`.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc as std_mpsc;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use ringbuf::traits::{Observer, Producer};
use tokio::sync::mpsc as tokio_mpsc;
use uuid::Uuid;
use video_merger_engine::audio::output::AudioOutput;
use video_merger_engine::audio::FfmpegAudioDecoder;
use video_merger_engine::decoder::{FfmpegMediaDecoder, MediaDecoder};

use crate::job::Event;

/// Trim bounds for the preview session.
#[derive(Debug, Clone, Copy)]
pub struct PreviewBounds {
    pub trim_in_us: i64,
    pub trim_out_us: i64,
}

#[derive(Debug)]
pub enum PreviewCtrl {
    Play,
    Pause,
    Seek { pts_us: i64 },
    Stop,
    SetMuted(bool),
    SetVolume(f32),
}

// ── Internal per-thread message types ─────────────────────────────────────

enum VideoCtrl {
    Play,
    Pause,
    Seek(i64),
    Stop,
}

enum AudioCtrl {
    Play,
    Pause,
    Seek(i64),
    Stop,
    SetMuted(bool),
    SetVolume(f32),
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
        }
    }
}

/// Spawn the preview threads, returning the control handle.
pub fn spawn_preview(
    clip_id: Uuid,
    path: PathBuf,
    bounds: PreviewBounds,
    event_tx: tokio_mpsc::UnboundedSender<Event>,
) -> PreviewHandle {
    // Try to set up audio first so we can hand the clock atomic to the video
    // thread. An open failure (no audio stream or device unavailable) is
    // non-fatal — preview falls back to silent + wall-clock pacing.
    let (audio_tx, audio_join, audio_clock) = match try_open_audio(&path, bounds) {
        Some((decoder, output)) => {
            let clock = output.frames_played_handle();
            let (tx, rx) = std_mpsc::channel::<AudioCtrl>();
            let bounds_a = bounds;
            let join = thread::Builder::new()
                .name("cutoff-preview-audio".into())
                .spawn(move || audio_loop(decoder, output, bounds_a, rx))
                .expect("audio preview thread spawn");
            (Some(tx), Some(join), Some(clock))
        }
        None => (None, None, None),
    };

    let (video_tx, video_rx) = std_mpsc::channel::<VideoCtrl>();
    let video_join = thread::Builder::new()
        .name("cutoff-preview-video".into())
        .spawn(move || {
            video_loop(clip_id, path, bounds, video_rx, event_tx, audio_clock);
        })
        .expect("video preview thread spawn");

    PreviewHandle {
        video_tx,
        audio_tx,
        _video_join: video_join,
        _audio_join: audio_join,
    }
}

// ── Audio thread ──────────────────────────────────────────────────────────

fn try_open_audio(
    path: &std::path::Path,
    bounds: PreviewBounds,
) -> Option<(FfmpegAudioDecoder, AudioOutput)> {
    let mut decoder = match FfmpegAudioDecoder::open(path) {
        Ok(Some(d)) => d,
        Ok(None) => return None, // silent footage
        Err(e) => {
            tracing::warn!(error = %e, "audio open failed");
            return None;
        }
    };
    let output = match AudioOutput::new() {
        Ok(o) => o,
        Err(e) => {
            tracing::warn!(error = %e, "audio output unavailable");
            return None;
        }
    };
    if bounds.trim_in_us > 0 {
        let _ = decoder.seek_to_us(bounds.trim_in_us);
    }
    Some((decoder, output))
}

fn audio_loop(
    mut decoder: FfmpegAudioDecoder,
    mut output: AudioOutput,
    bounds: PreviewBounds,
    ctrl_rx: std_mpsc::Receiver<AudioCtrl>,
) {
    let mut playing = false;
    /// Refill threshold (≈200 ms of stereo @ 48k).
    const TARGET_BUFFERED: usize = 19_200;
    /// Bulk batch (≈40 ms of stereo @ 48k) — amortizes per-sample atomic cost.
    const BATCH_SAMPLES: usize = 3_840;

    let mut staging: Vec<f32> = Vec::with_capacity(BATCH_SAMPLES);

    loop {
        // Drain controls. Block when paused (no audio work to do).
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
                    let target = (bounds.trim_in_us + pts_us)
                        .clamp(bounds.trim_in_us, bounds.trim_out_us - 1);
                    let _ = decoder.seek_to_us(target);
                    staging.clear();
                }
                AudioCtrl::Stop => return,
                AudioCtrl::SetMuted(m) => output.set_muted(m),
                AudioCtrl::SetVolume(v) => output.set_volume(v),
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

        // Push pending staged samples (carry-over from last iteration).
        if !staging.is_empty() {
            let pushed = output.producer.push_slice(&staging);
            staging.drain(..pushed);
        }

        // Pump until we have ≥200ms buffered or hit EOF/trim-out.
        let mut hit_end = false;
        while output.producer.vacant_len() > TARGET_BUFFERED {
            match decoder.next_frame() {
                Ok(Some(frame)) => {
                    if frame.pts_us >= bounds.trim_out_us {
                        hit_end = true;
                        break;
                    }
                    let pushed = output.producer.push_slice(&frame.samples);
                    if pushed < frame.samples.len() {
                        // Producer filled mid-batch — stash the rest.
                        staging.extend_from_slice(&frame.samples[pushed..]);
                        break;
                    }
                }
                Ok(None) => {
                    hit_end = true;
                    break;
                }
                Err(e) => {
                    tracing::warn!(error = %e, "audio decode failed");
                    break;
                }
            }
        }

        // Buffer healthy or EOF — yield CPU. cpal callback drains roughly
        // every 10–20ms so a 10ms sleep keeps us comfortably ahead.
        thread::sleep(if hit_end {
            Duration::from_millis(50)
        } else {
            Duration::from_millis(10)
        });
    }
}

// ── Video thread ──────────────────────────────────────────────────────────

fn video_loop(
    clip_id: Uuid,
    path: PathBuf,
    bounds: PreviewBounds,
    ctrl_rx: std_mpsc::Receiver<VideoCtrl>,
    event_tx: tokio_mpsc::UnboundedSender<Event>,
    audio_clock: Option<Arc<AtomicU32>>,
) {
    let mut decoder = match FfmpegMediaDecoder::open(&path) {
        Ok(d) => d,
        Err(e) => {
            let _ = event_tx.send(Event::Failed {
                id: clip_id,
                message: format!("open preview: {e}"),
            });
            return;
        }
    };

    let meta = decoder.meta();
    let effective_duration_us = (bounds.trim_out_us - bounds.trim_in_us).max(0);
    let _ = event_tx.send(Event::PreviewOpened {
        clip_id,
        duration_us: effective_duration_us,
        width: decoder.width(),
        height: decoder.height(),
        frame_rate_num: meta.frame_rate_num,
        frame_rate_den: meta.frame_rate_den,
    });

    if bounds.trim_in_us > 0 {
        let _ = decoder.seek_to_us(bounds.trim_in_us);
    }

    let mut last_delivered_pts_us: i64 = 0;
    if let Ok(Some(frame)) = decoder.next_frame() {
        let local = source_pts_to_local(frame.pts_us, bounds);
        last_delivered_pts_us = local;
        let _ = event_tx.send(Event::FrameReady {
            clip_id,
            frame: remap_pts(frame, local),
        });
    }

    let frame_interval = compute_frame_interval(&meta);
    let frame_interval_us = frame_interval.as_micros() as i64;
    let mut playing = false;
    let mut next_frame_due = Instant::now();
    let mut clock_anchor: Option<(u32, i64)> = None;

    loop {
        // Drain controls. Block when paused.
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
                    let target = (bounds.trim_in_us + pts_us)
                        .clamp(bounds.trim_in_us, bounds.trim_out_us - 1);
                    if let Err(e) = decoder.seek_to_us(target) {
                        tracing::warn!(error = %e, "preview seek failed");
                    } else if let Ok(Some(frame)) = decoder.next_frame() {
                        let local = source_pts_to_local(frame.pts_us, bounds);
                        last_delivered_pts_us = local;
                        let _ = event_tx.send(Event::FrameReady {
                            clip_id,
                            frame: remap_pts(frame, local),
                        });
                    }
                    next_frame_due = Instant::now();
                    clock_anchor = None;
                }
                VideoCtrl::Stop => {
                    let _ = event_tx.send(Event::PreviewEnded { clip_id });
                    return;
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
            // Audio-master clock pacing.
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

            // ── Smart seek when far behind ────────────────────────────────
            // Decode-and-drop is fine for a few frames; if we're > 500 ms
            // behind, seeking saves an arbitrary amount of decode work.
            if target_local_us - last_delivered_pts_us > 500_000 {
                let seek_source = (bounds.trim_in_us + target_local_us - 100_000)
                    .clamp(bounds.trim_in_us, bounds.trim_out_us - 1);
                if decoder.seek_to_us(seek_source).is_ok() {
                    last_delivered_pts_us = source_pts_to_local(seek_source, bounds);
                }
            }

            // Pull next frame; drop late ones (more than 2 frame intervals behind).
            let mut got: Option<video_merger_engine::DecodedFrame> = None;
            let drop_threshold_us = frame_interval_us * 2;
            loop {
                match decoder.next_frame() {
                    Ok(Some(frame)) => {
                        if frame.pts_us >= bounds.trim_out_us {
                            let _ = event_tx.send(Event::PreviewEnded { clip_id });
                            playing = false;
                            clock_anchor = None;
                            break;
                        }
                        let local = source_pts_to_local(frame.pts_us, bounds);
                        if local + drop_threshold_us < target_local_us {
                            continue;
                        }
                        got = Some(remap_pts(frame, local));
                        break;
                    }
                    Ok(None) => {
                        let _ = event_tx.send(Event::PreviewEnded { clip_id });
                        playing = false;
                        clock_anchor = None;
                        break;
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "decode error during playback");
                        let _ = event_tx.send(Event::PreviewEnded { clip_id });
                        playing = false;
                        clock_anchor = None;
                        break;
                    }
                }
            }
            if let Some(frame) = got {
                let wait_us = frame.pts_us - target_local_us;
                if wait_us > 1_500 {
                    let sleep_us = wait_us.min(100_000) as u64;
                    thread::sleep(Duration::from_micros(sleep_us));
                }
                last_delivered_pts_us = frame.pts_us;
                if event_tx.send(Event::FrameReady { clip_id, frame }).is_err() {
                    return;
                }
            }
            continue;
        }

        // ── Fallback: wall-clock pacing (no audio) ────────────────────────
        let now = Instant::now();
        if now < next_frame_due {
            thread::sleep(next_frame_due - now);
        }
        next_frame_due += frame_interval;

        match decoder.next_frame() {
            Ok(Some(frame)) => {
                if frame.pts_us >= bounds.trim_out_us {
                    let _ = event_tx.send(Event::PreviewEnded { clip_id });
                    playing = false;
                    continue;
                }
                let local = source_pts_to_local(frame.pts_us, bounds);
                last_delivered_pts_us = local;
                if event_tx
                    .send(Event::FrameReady {
                        clip_id,
                        frame: remap_pts(frame, local),
                    })
                    .is_err()
                {
                    return;
                }
            }
            Ok(None) => {
                let _ = event_tx.send(Event::PreviewEnded { clip_id });
                playing = false;
            }
            Err(e) => {
                tracing::warn!(error = %e, "decode error during playback");
                let _ = event_tx.send(Event::PreviewEnded { clip_id });
                playing = false;
            }
        }
    }
}

// ── Shared helpers ────────────────────────────────────────────────────────

fn compute_frame_interval(meta: &video_merger_engine::decoder::StreamMeta) -> Duration {
    let fps = meta.frame_rate_f32();
    if fps > 1.0 && fps < 240.0 {
        Duration::from_secs_f32(1.0 / fps)
    } else {
        Duration::from_millis(33) // ~30fps fallback
    }
}

fn source_pts_to_local(source_pts_us: i64, bounds: PreviewBounds) -> i64 {
    (source_pts_us - bounds.trim_in_us).max(0)
}

fn remap_pts(
    frame: video_merger_engine::decoder::DecodedFrame,
    new_pts_us: i64,
) -> video_merger_engine::decoder::DecodedFrame {
    video_merger_engine::decoder::DecodedFrame {
        pts_us: new_pts_us,
        ..frame
    }
}
