use std::path::PathBuf;
use std::time::SystemTime;

use slint::{ComponentHandle, Image, SharedPixelBuffer, SharedString, Weak};
use tokio::sync::mpsc;
use uuid::Uuid;
use video_merger_core::domain::Clip;
use video_merger_engine::DecodedFrame;
use video_merger_persistence::ExportRecord;
use video_merger_ui::AppWindow;
use video_merger_worker::{Command, Event, JobId};

use super::models;
use super::timeline_view;
use super::{BridgeState, JobMeta};

/// Bridge worker events back into Slint property updates.
pub fn install(
    window: &AppWindow,
    mut event_rx: mpsc::UnboundedReceiver<Event>,
    cmd_tx: mpsc::Sender<Command>,
    state: BridgeState,
) {
    let weak: Weak<AppWindow> = window.as_weak();
    std::thread::Builder::new()
        .name("ui-event-bridge".into())
        .spawn(move || {
            while let Some(event) = event_rx.blocking_recv() {
                let weak = weak.clone();
                let st = state.clone();
                let tx = cmd_tx.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(window) = weak.upgrade() {
                        apply(&window, event, &st, &tx);
                    }
                });
            }
        })
        .expect("ui-event-bridge thread");
}

fn apply(window: &AppWindow, event: Event, state: &BridgeState, cmd_tx: &mpsc::Sender<Command>) {
    match event {
        Event::Probed { path, info, .. } => {
            let clip = Clip::new(path.clone(), info);
            let clip_id = clip.id;
            let data = models::clip_to_data(&clip);
            let name = data.name.clone();
            {
                let mut pl = state.playlist.lock().expect("playlist mutex poisoned");
                // Snapshot before mutation so Cmd+Z removes this clip.
                if let Ok(mut u) = state.undo.lock() {
                    u.checkpoint(&pl);
                }
                pl.push(clip);
            }
            models::push_clip(window, data);
            window.set_status_text(format!("Added: {name}").into());
            refresh_ruler_from_state(window, state);

            // Kick off thumbnail + waveform extraction so the card fills in.
            let out_dir = thumbs_dir(&state.storage.data_dir, clip_id);
            enqueue_thumbnails(cmd_tx, clip_id, path.clone(), out_dir);
            let wf_path = waveform_path(&state.storage.data_dir, clip_id);
            enqueue_waveform(cmd_tx, clip_id, path, wf_path);

            // Sync preview + refresh per-track lanes in the UI.
            let project = {
                let pl = state.playlist.lock().expect("playlist mutex poisoned");
                let ts = state.tracks.lock().expect("tracks mutex poisoned");
                let mut proj = state.project.lock().expect("project mutex poisoned");
                proj.populate_from_playlist(&pl);
                for (idx, track) in proj.tracks.iter_mut().enumerate() {
                    track.muted = ts.muted.get(idx).copied().unwrap_or(false);
                    track.solo = ts.soloed.get(idx).copied().unwrap_or(false);
                    track.locked = ts.locked.get(idx).copied().unwrap_or(false);
                }
                models::sync_tracks(window, &proj, &ts);
                proj.clone()
            };
            let _ = cmd_tx.try_send(Command::OpenPreview { project });
        }
        Event::ClipReady(clip) => {
            let path = clip.path.clone();
            let clip_id = clip.id;
            let data = models::clip_to_data(&clip);
            {
                let mut pl = state.playlist.lock().expect("playlist mutex poisoned");
                if let Ok(mut u) = state.undo.lock() {
                    u.checkpoint(&pl);
                }
                pl.push(clip);
            }
            models::push_clip(window, data);
            refresh_ruler_from_state(window, state);
            let out_dir = thumbs_dir(&state.storage.data_dir, clip_id);
            enqueue_thumbnails(cmd_tx, clip_id, path.clone(), out_dir);
            let wf_path = waveform_path(&state.storage.data_dir, clip_id);
            enqueue_waveform(cmd_tx, clip_id, path, wf_path);

            // Sync preview + refresh per-track lanes in the UI.
            let project = {
                let pl = state.playlist.lock().expect("playlist mutex poisoned");
                let ts = state.tracks.lock().expect("tracks mutex poisoned");
                let mut proj = state.project.lock().expect("project mutex poisoned");
                proj.populate_from_playlist(&pl);
                for (idx, track) in proj.tracks.iter_mut().enumerate() {
                    track.muted = ts.muted.get(idx).copied().unwrap_or(false);
                    track.solo = ts.soloed.get(idx).copied().unwrap_or(false);
                    track.locked = ts.locked.get(idx).copied().unwrap_or(false);
                }
                models::sync_tracks(window, &proj, &ts);
                proj.clone()
            };
            let _ = cmd_tx.try_send(Command::OpenPreview { project });
        }
        Event::Progress { fraction, .. } => window.set_export_progress(fraction),
        Event::Finished { id, output } => {
            let meta = take_meta(state, id);
            clear_active(state);
            window.set_exporting(false);
            window.set_export_progress(1.0);
            window.set_status_text(format!("Export complete: {}", output.display()).into());

            if let Some(meta) = meta {
                record_export(state, &meta);
                update_default_output_dir(state, &meta.output_path);
            }
        }
        Event::Failed { id, message } => {
            let _ = take_meta(state, id);
            clear_active(state);
            window.set_exporting(false);
            tracing::warn!(%message, "job failed");
            window.set_status_text(format!("Failed: {message}").into());
            show_toast(window, &message);
        }
        Event::Cancelled { id } => {
            let _ = take_meta(state, id);
            clear_active(state);
            window.set_exporting(false);
            window.set_status_text("Cancelled".into());
        }
        Event::PreviewOpened {
            duration_us,
            ..
        } => {
            let pending = {
                let mut pv = state.preview.lock().expect("preview mutex poisoned");
                pv.duration_us = duration_us;
                pv.pending_seek_us.take()
            };
            if let Some(us) = pending {
                if let Err(err) = cmd_tx.try_send(Command::SeekPreview { pts_us: us }) {
                    tracing::warn!(error = %err, "deferred seek failed");
                }
            }
        }
        Event::FrameReady { frame, .. } => {
            let fps = {
                let mut pv = state.preview.lock().expect("preview mutex poisoned");
                pv.playhead_us = frame.pts_us;
                // Rolling FPS over the last 1 second.
                let now = std::time::Instant::now();
                pv.frame_history.push_back(now);
                let one_sec = std::time::Duration::from_secs(1);
                while let Some(t) = pv.frame_history.front() {
                    if now.duration_since(*t) > one_sec {
                        pv.frame_history.pop_front();
                    } else {
                        break;
                    }
                }
                pv.frame_history.len() as f32
            };
            window.set_preview_fps(fps);
            // Timeline-global playhead: frame.pts_us is already timeline-absolute!
            let (global_fraction, global_us) = {
                let pl = state.playlist.lock().expect("playlist mutex poisoned");
                let total_us = timeline_view::total_duration_us(&pl);
                let abs_us = frame.pts_us;
                if total_us > 0 {
                    ((abs_us as f64 / total_us as f64) as f32, abs_us)
                } else {
                    (0.0, abs_us)
                }
            };
            window.set_preview_frame(frame_to_image(&frame));
            window.set_playhead_text(SharedString::from(
                crate::view_model::timeline_vm::format_us(global_us),
            ));
            window.set_playhead_fraction(global_fraction.clamp(0.0, 1.0));
        }
        Event::PreviewEnded { .. } => {
            {
                let mut pv = state.preview.lock().expect("preview mutex poisoned");
                pv.playing = false;
                pv.frame_history.clear();
            }
            window.set_playing(false);
            window.set_preview_fps(0.0);
        }
        Event::ThumbnailsReady { clip_id, paths } => {
            let pl = {
                let mut pl = state.playlist.lock().expect("playlist mutex poisoned");
                for clip in pl.clips_mut() {
                    if clip.id == clip_id {
                        clip.thumbnails = paths.clone();
                        break;
                    }
                }
                pl.clone()
            };
            models::refresh_clip_thumbnails(window, &pl, clip_id);
        }
        Event::WaveformReady { clip_id, path } => {
            let pl = {
                let mut pl = state.playlist.lock().expect("playlist mutex poisoned");
                for clip in pl.clips_mut() {
                    if clip.id == clip_id {
                        clip.waveform_path = Some(path.clone());
                        break;
                    }
                }
                pl.clone()
            };
            models::refresh_clip_thumbnails(window, &pl, clip_id);
        }
    }
}

fn frame_to_image(frame: &DecodedFrame) -> Image {
    let buf = SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(
        &frame.rgba,
        frame.width,
        frame.height,
    );
    Image::from_rgba8(buf)
}

/// Show a toast for 4 seconds. Timer is intentionally leaked because Slint's
/// `Timer` must be kept alive for it to fire — it self-disposes after the
/// single-shot tick when the closure drops it.
fn show_toast(window: &AppWindow, message: &str) {
    window.set_toast_message(SharedString::from(message));
    window.set_toast_visible(true);
    let weak = window.as_weak();
    let timer = std::rc::Rc::new(slint::Timer::default());
    let timer_for_cb = timer.clone();
    timer.start(
        slint::TimerMode::SingleShot,
        std::time::Duration::from_secs(4),
        move || {
            if let Some(w) = weak.upgrade() {
                w.set_toast_visible(false);
            }
            // Drop the inner clone now that we've fired so the Timer is collected.
            let _ = &timer_for_cb;
        },
    );
    // Leak the Rc so the timer outlives this stack frame; the closure clone keeps
    // it alive while the timer is pending, and dropping it after fire cleans up.
    Box::leak(Box::new(timer));
}

fn thumbs_dir(data_dir: &std::path::Path, clip_id: Uuid) -> PathBuf {
    data_dir.join("thumbs").join(clip_id.to_string())
}

/// Recompute and push ruler ticks + total width using the bridge's current zoom.
fn refresh_ruler_from_state(window: &AppWindow, state: &BridgeState) {
    let zoom = *state.zoom.lock().expect("zoom mutex poisoned");
    let pl = state.playlist.lock().expect("playlist mutex poisoned");
    timeline_view::refresh_ruler(window, &pl, zoom);
}

fn enqueue_thumbnails(
    cmd_tx: &mpsc::Sender<Command>,
    clip_id: Uuid,
    path: PathBuf,
    out_dir: PathBuf,
) {
    if let Err(err) = cmd_tx.try_send(Command::GenerateThumbnails {
        clip_id,
        path,
        out_dir,
        count: 10,
    }) {
        tracing::warn!(error = %err, "failed to enqueue thumbnails");
    }
}

fn waveform_path(data_dir: &std::path::Path, clip_id: Uuid) -> PathBuf {
    data_dir.join("waveforms").join(format!("{clip_id}.bin"))
}

fn enqueue_waveform(
    cmd_tx: &mpsc::Sender<Command>,
    clip_id: Uuid,
    path: PathBuf,
    out_path: PathBuf,
) {
    if let Err(err) = cmd_tx.try_send(Command::GenerateWaveform {
        clip_id,
        path,
        out_path,
    }) {
        tracing::warn!(error = %err, "failed to enqueue waveform");
    }
}

fn clear_active(state: &BridgeState) {
    *state.active_job.lock().expect("active_job mutex poisoned") = None;
}

fn take_meta(state: &BridgeState, id: JobId) -> Option<JobMeta> {
    state
        .job_meta
        .lock()
        .expect("job_meta mutex poisoned")
        .remove(&id)
}

fn record_export(state: &BridgeState, meta: &JobMeta) {
    let record = ExportRecord {
        output_path: meta.output_path.clone(),
        input_count: meta.input_count,
        finished_at: SystemTime::now(),
        duration_secs: meta.duration.as_secs_f64(),
    };
    let mut history = state.history.lock().expect("history mutex poisoned");
    history.push(record);
    if let Err(err) = history.save(&state.storage) {
        tracing::warn!(error = %err, "failed to save history");
    }
}

fn update_default_output_dir(state: &BridgeState, output_path: &std::path::Path) {
    let Some(parent) = output_path.parent() else {
        return;
    };
    if parent.as_os_str().is_empty() {
        return;
    }
    let mut config = state.config.lock().expect("config mutex poisoned");
    if config.default_output_dir.as_deref() == Some(parent) {
        return;
    }
    config.default_output_dir = Some(parent.to_path_buf());
    if let Err(err) = config.save(&state.storage) {
        tracing::warn!(error = %err, "failed to save config");
    }
}

