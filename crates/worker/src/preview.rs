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
    if let Ok(Some(frame)) = decoder.next_frame() {
        let local = source_pts_to_local(frame.pts_us, bounds);
        let _ = event_tx.send(Event::FrameReady {
            clip_id,
            frame: remap_pts(frame, local),
        });
    }

    let frame_interval = compute_frame_interval(&meta);
    let mut playing = false;
    let mut next_frame_due = Instant::now();

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
                }
                PreviewCtrl::Pause => playing = false,
                PreviewCtrl::Seek { pts_us } => {
                    // pts_us arrives in clip-local time (0 = trim_in). Convert
                    // to source time before seeking.
                    let target = (bounds.trim_in_us + pts_us)
                        .clamp(bounds.trim_in_us, bounds.trim_out_us - 1);
                    if let Err(e) = decoder.seek_to_us(target) {
                        tracing::warn!(error = %e, "preview seek failed");
                    } else if let Ok(Some(frame)) = decoder.next_frame() {
                        let local = source_pts_to_local(frame.pts_us, bounds);
                        let _ = event_tx.send(Event::FrameReady {
                            clip_id,
                            frame: remap_pts(frame, local),
                        });
                    }
                    next_frame_due = Instant::now();
                }
                PreviewCtrl::Stop => {
                    let _ = event_tx.send(Event::PreviewEnded { clip_id });
                    return;
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

        // Pace frame delivery to native fps.
        let now = Instant::now();
        if now < next_frame_due {
            thread::sleep(next_frame_due - now);
        }
        next_frame_due += frame_interval;

        match decoder.next_frame() {
            Ok(Some(frame)) => {
                // Past trim-out → end the preview.
                if frame.pts_us >= bounds.trim_out_us {
                    let _ = event_tx.send(Event::PreviewEnded { clip_id });
                    playing = false;
                    continue;
                }
                let local = source_pts_to_local(frame.pts_us, bounds);
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
