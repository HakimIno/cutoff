//! Background preview-playback thread.
//!
//! Owns one `FfmpegMediaDecoder` at a time (decoder is `!Send`, so the thread
//! is dedicated). Control messages arrive over a `std::sync::mpsc` channel;
//! decoded frames are forwarded to the worker's UI-bound event channel.

use std::path::PathBuf;
use std::sync::mpsc as std_mpsc;
use std::thread;
use std::time::{Duration, Instant};

use tokio::sync::mpsc as tokio_mpsc;
use uuid::Uuid;
use video_merger_engine::audio::output::AudioOutput;
use video_merger_engine::audio::FfmpegAudioDecoder;
use video_merger_engine::decoder::{FfmpegMediaDecoder, MediaDecoder};
use ringbuf::traits::{Observer, Producer};

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

pub struct PreviewHandle {
    pub ctrl_tx: std_mpsc::Sender<PreviewCtrl>,
    _join: thread::JoinHandle<()>,
}

impl PreviewHandle {
    pub fn send(&self, ctrl: PreviewCtrl) {
        let _ = self.ctrl_tx.send(ctrl);
    }
}

/// Spawn the preview thread, returning its control handle.
///
/// Emits `PreviewOpened` on success, then `FrameReady` events while playing,
/// and `PreviewEnded` when EOF or `Stop`. On open failure, emits `Failed`.
pub fn spawn_preview(
    clip_id: Uuid,
    path: PathBuf,
    bounds: PreviewBounds,
    event_tx: tokio_mpsc::UnboundedSender<Event>,
) -> PreviewHandle {
    let (ctrl_tx, ctrl_rx) = std_mpsc::channel::<PreviewCtrl>();

    let join = thread::Builder::new()
        .name("video-merger-preview".into())
        .spawn(move || run_preview(clip_id, path, bounds, ctrl_rx, event_tx))
        .expect("preview thread spawn");

    PreviewHandle {
        ctrl_tx,
        _join: join,
    }
}

fn run_preview(
    clip_id: Uuid,
    path: PathBuf,
    bounds: PreviewBounds,
    ctrl_rx: std_mpsc::Receiver<PreviewCtrl>,
    event_tx: tokio_mpsc::UnboundedSender<Event>,
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

    // Try to open audio alongside video. Silent footage is OK — None just
    // means "video-only preview".
    let mut audio_decoder = match FfmpegAudioDecoder::open(&path) {
        Ok(opt) => opt,
        Err(e) => {
            tracing::warn!(error = %e, ?path, "audio open failed; preview will be silent");
            None
        }
    };
    // Spin up cpal output only when audio exists. The output keeps its own
    // stream alive for the lifetime of this preview session.
    let mut audio_out = if audio_decoder.is_some() {
        match AudioOutput::new() {
            Ok(o) => Some(o),
            Err(e) => {
                tracing::warn!(error = %e, "audio output unavailable; silent preview");
                audio_decoder = None;
                None
            }
        }
    } else {
        None
    };
    if let Some(ad) = audio_decoder.as_mut() {
        if bounds.trim_in_us > 0 {
            let _ = ad.seek_to_us(bounds.trim_in_us);
        }
    }

    let meta = decoder.meta();
    // Use clip-wall duration_us (post-trim window) so the bridge can map
    // playhead correctly.
    let effective_duration_us = (bounds.trim_out_us - bounds.trim_in_us).max(0);
    let _ = event_tx.send(Event::PreviewOpened {
        clip_id,
        duration_us: effective_duration_us,
        width: decoder.width(),
        height: decoder.height(),
        frame_rate_num: meta.frame_rate_num,
        frame_rate_den: meta.frame_rate_den,
    });

    // Seek into trim window so the first frame is the trimmed-in point.
    if bounds.trim_in_us > 0 {
        let _ = decoder.seek_to_us(bounds.trim_in_us);
    }

    // Deliver first frame so the user sees something on selection.
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
    // Audio-master clock anchor: `(frames_played_at_anchor, clip_local_pts_at_anchor)`.
    // Reset on Play/Pause/Seek so cpal's running silence-counter during pause
    // doesn't get baked into the timing.
    let mut clock_anchor: Option<(u32, i64)> = None;

    loop {
        // Drain controls. When paused, block until a control arrives or channel hangs up.
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

        let mut maybe_ctrl = first_ctrl;
        while let Some(ctrl) = maybe_ctrl.take() {
            match ctrl {
                PreviewCtrl::Play => {
                    playing = true;
                    next_frame_due = Instant::now();
                    clock_anchor = None;
                }
                PreviewCtrl::Pause => {
                    playing = false;
                    clock_anchor = None;
                }
                PreviewCtrl::Seek { pts_us } => {
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
                    if let Some(ad) = audio_decoder.as_mut() {
                        let _ = ad.seek_to_us(target);
                    }
                    next_frame_due = Instant::now();
                    clock_anchor = None;
                }
                PreviewCtrl::Stop => {
                    let _ = event_tx.send(Event::PreviewEnded { clip_id });
                    return;
                }
                PreviewCtrl::SetMuted(m) => {
                    if let Some(o) = &audio_out {
                        o.set_muted(m);
                    }
                }
                PreviewCtrl::SetVolume(v) => {
                    if let Some(o) = &audio_out {
                        o.set_volume(v);
                    }
                }
            }
            match ctrl_rx.try_recv() {
                Ok(c) => maybe_ctrl = Some(c),
                Err(std_mpsc::TryRecvError::Empty) => break,
                Err(std_mpsc::TryRecvError::Disconnected) => return,
            }
        }

        if !playing {
            continue;
        }

        // Keep the audio ring buffer ahead — pump until ≥200ms buffered or EOF.
        if let (Some(ad), Some(ao)) = (audio_decoder.as_mut(), audio_out.as_mut()) {
            // Each audio frame is ~ N samples × 2 channels. Push until producer
            // capacity drops below a low-water mark.
            const TARGET_BUFFERED: usize = 19_200; // ≈ 200ms @ 48k stereo
            while ao.producer.vacant_len() > TARGET_BUFFERED {
                match ad.next_frame() {
                    Ok(Some(frame)) => {
                        // Bail when audio passes trim-out so we don't overshoot.
                        if frame.pts_us >= bounds.trim_out_us {
                            break;
                        }
                        for s in &frame.samples {
                            if ao.producer.try_push(*s).is_err() {
                                break;
                            }
                        }
                    }
                    Ok(None) => break,
                    Err(e) => {
                        tracing::warn!(error = %e, "audio decode failed");
                        break;
                    }
                }
            }
        }

        // Two pacing strategies:
        //   * If we have an audio output, video PTS is locked to audio time
        //     (frames_played / 48_000). Frames decoded ahead are slept on;
        //     frames decoded behind by more than one frame interval are dropped.
        //   * Otherwise (silent video / audio open failed), fall back to wall
        //     clock with `next_frame_due += frame_interval`.
        if let Some(ao) = audio_out.as_ref() {
            let anchor = match clock_anchor {
                Some(a) => a,
                None => {
                    let a = (ao.frames_played(), last_delivered_pts_us);
                    clock_anchor = Some(a);
                    a
                }
            };
            let frames_now = ao.frames_played();
            let elapsed_frames = frames_now.wrapping_sub(anchor.0) as i64;
            let elapsed_us = elapsed_frames * 1_000_000
                / video_merger_engine::audio::SAMPLE_RATE as i64;
            let target_local_us = anchor.1 + elapsed_us;

            // Pull next frame; drop late ones (more than 2 frame_intervals behind).
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
                            continue; // late → drop
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
                // Sleep if the frame would be ahead of the audio clock.
                let wait_us = frame.pts_us - target_local_us;
                if wait_us > 1_500 {
                    // Cap sleep at 100ms to remain responsive to ctrl messages.
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

        // ───────── Fallback: wall-clock pacing (no audio) ─────────
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

fn compute_frame_interval(meta: &video_merger_engine::decoder::StreamMeta) -> Duration {
    let fps = meta.frame_rate_f32();
    if fps > 1.0 && fps < 240.0 {
        Duration::from_secs_f32(1.0 / fps)
    } else {
        Duration::from_millis(33) // ~30fps fallback
    }
}
