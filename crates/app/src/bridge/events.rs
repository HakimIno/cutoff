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
                pl.push(clip);
            }
            models::push_clip(window, data);
            window.set_status_text(format!("Added: {name}").into());

            // Kick off thumbnail extraction so the filmstrip fills in.
            let out_dir = thumbs_dir(&state.storage.data_dir, clip_id);
            enqueue_thumbnails(cmd_tx, clip_id, path, out_dir);
        }
        Event::ClipReady(clip) => {
            let path = clip.path.clone();
            let clip_id = clip.id;
            let data = models::clip_to_data(&clip);
            {
                let mut pl = state.playlist.lock().expect("playlist mutex poisoned");
                pl.push(clip);
            }
            models::push_clip(window, data);
            let out_dir = thumbs_dir(&state.storage.data_dir, clip_id);
            enqueue_thumbnails(cmd_tx, clip_id, path, out_dir);
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
        }
        Event::Cancelled { id } => {
            let _ = take_meta(state, id);
            clear_active(state);
            window.set_exporting(false);
            window.set_status_text("Cancelled".into());
        }
        Event::PreviewOpened {
            clip_id,
            duration_us,
            ..
        } => {
            let mut pv = state.preview.lock().expect("preview mutex poisoned");
            if pv.clip_id != Some(clip_id) {
                return;
            }
            pv.duration_us = duration_us;
        }
        Event::FrameReady { clip_id, frame } => {
            let (matches, duration_us) = {
                let mut pv = state.preview.lock().expect("preview mutex poisoned");
                if pv.clip_id != Some(clip_id) {
                    (false, 0)
                } else {
                    pv.playhead_us = frame.pts_us;
                    (true, pv.duration_us)
                }
            };
            if !matches {
                return;
            }
            window.set_preview_frame(frame_to_image(&frame));
            window.set_playhead_text(SharedString::from(
                crate::view_model::timeline_vm::format_us(frame.pts_us),
            ));
            if duration_us > 0 {
                let f = (frame.pts_us as f64 / duration_us as f64) as f32;
                window.set_playhead_fraction(f.clamp(0.0, 1.0));
            }
        }
        Event::PreviewEnded { clip_id } => {
            let matches = {
                let mut pv = state.preview.lock().expect("preview mutex poisoned");
                if pv.clip_id != Some(clip_id) {
                    false
                } else {
                    pv.playing = false;
                    true
                }
            };
            if matches {
                window.set_playing(false);
            }
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

fn thumbs_dir(data_dir: &std::path::Path, clip_id: Uuid) -> PathBuf {
    data_dir.join("thumbs").join(clip_id.to_string())
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

