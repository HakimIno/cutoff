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
    event_tx: tokio_mpsc::UnboundedSender<Event>,
) -> PreviewHandle {
    let (ctrl_tx, ctrl_rx) = std_mpsc::channel::<PreviewCtrl>();

    let join = thread::Builder::new()
        .name("video-merger-preview".into())
        .spawn(move || run_preview(clip_id, path, ctrl_rx, event_tx))
        .expect("preview thread spawn");

    PreviewHandle {
        ctrl_tx,
        _join: join,
    }
}

fn run_preview(
    clip_id: Uuid,
    path: PathBuf,
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
    let _ = event_tx.send(Event::PreviewOpened {
        clip_id,
        duration_us: meta.duration_us,
        width: decoder.width(),
        height: decoder.height(),
        frame_rate_num: meta.frame_rate_num,
        frame_rate_den: meta.frame_rate_den,
    });

    // Deliver first frame so the user sees something on selection.
    if let Ok(Some(frame)) = decoder.next_frame() {
        let _ = event_tx.send(Event::FrameReady { clip_id, frame });
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
                    if let Err(e) = decoder.seek_to_us(pts_us) {
                        tracing::warn!(error = %e, "preview seek failed");
                    } else if let Ok(Some(frame)) = decoder.next_frame() {
                        let _ = event_tx.send(Event::FrameReady { clip_id, frame });
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
                if event_tx.send(Event::FrameReady { clip_id, frame }).is_err() {
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

fn compute_frame_interval(meta: &video_merger_engine::decoder::StreamMeta) -> Duration {
    let fps = meta.frame_rate_f32();
    if fps > 1.0 && fps < 240.0 {
        Duration::from_secs_f32(1.0 / fps)
    } else {
        Duration::from_millis(33) // ~30fps fallback
    }
}
