use std::path::PathBuf;

use slint::{ComponentHandle, SharedString, Weak};
use tokio::sync::mpsc;
use uuid::Uuid;
use video_merger_core::domain::{ContainerFormat, ExportSpec, Quality};
use video_merger_core::services::MergePlanner;
use video_merger_ui::AppWindow;
use video_merger_worker::Command;

use super::{models, timeline_view};
use super::{
    ActiveJob, BridgeState, JobMeta, JobMetaMap, SharedConfig, SharedPlaylist, SharedPreview,
    SharedZoom, ZOOM_MAX, ZOOM_MIN,
};

const DROP_TARGET_NONE: i32 = -1;

pub fn install(window: &AppWindow, cmd_tx: mpsc::Sender<Command>, state: BridgeState) {
    {
        let tx = cmd_tx.clone();
        let weak = window.as_weak();
        window.on_browse_clicked(move || on_browse(weak.clone(), tx.clone()));
    }
    {
        let weak = window.as_weak();
        let pl = state.playlist.clone();
        let zoom = state.zoom.clone();
        window.on_remove_clicked(move |id| {
            on_remove(weak.clone(), pl.clone(), zoom.clone(), id)
        });
    }
    {
        let weak = window.as_weak();
        window.on_drag_started(move |idx| on_drag_started(weak.clone(), idx));
    }
    {
        let weak = window.as_weak();
        let pl = state.playlist.clone();
        let zoom = state.zoom.clone();
        window.on_drag_moved(move |idx, dx| {
            on_drag_moved(weak.clone(), pl.clone(), zoom.clone(), idx, dx)
        });
    }
    {
        let weak = window.as_weak();
        let pl = state.playlist.clone();
        let zoom = state.zoom.clone();
        window.on_drag_released(move |idx, dx| {
            on_drag_released(weak.clone(), pl.clone(), zoom.clone(), idx, dx)
        });
    }
    {
        let tx = cmd_tx.clone();
        let weak = window.as_weak();
        let pl = state.playlist.clone();
        let pv = state.preview.clone();
        window.on_clip_selected(move |id| {
            let Ok(uuid) = Uuid::parse_str(id.as_str()) else { return };
            select_and_open(weak.clone(), tx.clone(), pl.clone(), pv.clone(), uuid, None);
        });
    }
    {
        let tx = cmd_tx.clone();
        let weak = window.as_weak();
        let pv = state.preview.clone();
        window.on_play_toggle(move || on_play_toggle(weak.clone(), tx.clone(), pv.clone()));
    }
    {
        let tx = cmd_tx.clone();
        let weak = window.as_weak();
        let pl = state.playlist.clone();
        let pv = state.preview.clone();
        window.on_seek_fraction(move |f| {
            on_seek_fraction(weak.clone(), tx.clone(), pl.clone(), pv.clone(), f)
        });
    }
    {
        let weak = window.as_weak();
        let pl = state.playlist.clone();
        let zoom = state.zoom.clone();
        window.on_zoom_in(move || on_zoom(weak.clone(), pl.clone(), zoom.clone(), 1.5));
    }
    {
        let weak = window.as_weak();
        let pl = state.playlist.clone();
        let zoom = state.zoom.clone();
        window.on_zoom_out(move || on_zoom(weak.clone(), pl.clone(), zoom.clone(), 1.0 / 1.5));
    }
    {
        let tx = cmd_tx.clone();
        let pv = state.preview.clone();
        window.on_jump_start(move || on_jump(tx.clone(), pv.clone(), JumpTo::Start));
    }
    {
        let tx = cmd_tx.clone();
        let pv = state.preview.clone();
        window.on_jump_end(move || on_jump(tx.clone(), pv.clone(), JumpTo::End));
    }
    {
        let tx = cmd_tx.clone();
        let weak = window.as_weak();
        let pl = state.playlist.clone();
        let aj = state.active_job.clone();
        let jm = state.job_meta.clone();
        let cfg = state.config.clone();
        window.on_export_clicked(move || {
            on_export(
                weak.clone(),
                tx.clone(),
                pl.clone(),
                aj.clone(),
                jm.clone(),
                cfg.clone(),
            )
        });
    }
    {
        let tx = cmd_tx;
        let aj = state.active_job.clone();
        window.on_cancel_clicked(move || on_cancel(tx.clone(), aj.clone()));
    }
}

enum JumpTo {
    Start,
    End,
}

fn on_browse(weak: Weak<AppWindow>, cmd_tx: mpsc::Sender<Command>) {
    let paths = rfd::FileDialog::new()
        .add_filter("Video", &["mp4", "mov", "mkv", "avi", "webm", "m4v"])
        .set_title("Add video clips")
        .pick_files();

    let Some(paths) = paths else { return };

    if let Some(window) = weak.upgrade() {
        window.set_status_text(format!("Probing {} file(s)…", paths.len()).into());
    }

    for path in paths {
        let cmd = Command::Probe {
            id: Uuid::new_v4(),
            path,
        };
        if let Err(err) = cmd_tx.try_send(cmd) {
            tracing::warn!(error = %err, "failed to enqueue probe command");
        }
    }
}

fn on_remove(
    weak: Weak<AppWindow>,
    playlist: SharedPlaylist,
    zoom: SharedZoom,
    id: SharedString,
) {
    let Ok(uuid) = Uuid::parse_str(id.as_str()) else {
        tracing::warn!(%id, "remove: invalid clip id");
        return;
    };

    let mut pl = playlist.lock().expect("playlist mutex poisoned");
    if let Err(err) = pl.remove(uuid) {
        tracing::warn!(error = %err, "remove failed");
        return;
    }
    if let Some(window) = weak.upgrade() {
        models::sync_clips(&window, &pl);
        let z = *zoom.lock().expect("zoom mutex poisoned");
        timeline_view::refresh_ruler(&window, &pl, z);
        window.set_status_text(format!("{} clips in timeline", pl.len()).into());
    }
}

fn on_drag_started(weak: Weak<AppWindow>, idx: i32) {
    if let Some(window) = weak.upgrade() {
        window.set_drag_from_index(idx);
        window.set_drop_target_index(DROP_TARGET_NONE);
    }
}

fn on_drag_moved(
    weak: Weak<AppWindow>,
    playlist: SharedPlaylist,
    zoom: SharedZoom,
    idx: i32,
    delta_px: f32,
) {
    if idx < 0 {
        return;
    }
    let z = *zoom.lock().expect("zoom mutex poisoned");
    let pl = playlist.lock().expect("playlist mutex poisoned");
    let Some((target_idx, indicator_x, _reorder_to)) =
        timeline_view::drop_target(&pl, z, idx as usize, delta_px)
    else {
        return;
    };
    if let Some(window) = weak.upgrade() {
        window.set_drop_target_index(target_idx as i32);
        window.set_drop_indicator_x(indicator_x);
    }
}

fn on_drag_released(
    weak: Weak<AppWindow>,
    playlist: SharedPlaylist,
    zoom: SharedZoom,
    idx: i32,
    delta_px: f32,
) {
    let Some(window) = weak.upgrade() else { return };
    window.set_drag_from_index(DROP_TARGET_NONE);
    window.set_drop_target_index(DROP_TARGET_NONE);

    if idx < 0 {
        return;
    }
    let z = *zoom.lock().expect("zoom mutex poisoned");
    let mut pl = playlist.lock().expect("playlist mutex poisoned");
    let Some((_target_idx, _indicator_x, reorder_to)) =
        timeline_view::drop_target(&pl, z, idx as usize, delta_px)
    else {
        return;
    };
    let Some(to) = reorder_to else { return };
    if let Err(err) = pl.reorder(idx as usize, to) {
        tracing::warn!(error = %err, "reorder failed");
        return;
    }
    models::sync_clips(&window, &pl);
    timeline_view::refresh_ruler(&window, &pl, z);
}

/// Select `clip_id`, push its metadata to the UI, and open a preview session.
/// `initial_seek_us` is stored as a pending seek so the first
/// `PreviewOpened` event triggers a `SeekPreview` automatically.
fn select_and_open(
    weak: Weak<AppWindow>,
    cmd_tx: mpsc::Sender<Command>,
    playlist: SharedPlaylist,
    preview: SharedPreview,
    clip_id: Uuid,
    initial_seek_us: Option<i64>,
) {
    let (path, name, codec, resolution, duration, fps, trim_in_us, trim_out_us) = {
        let pl = playlist.lock().expect("playlist mutex poisoned");
        let Some(clip) = pl.clips().iter().find(|c| c.id == clip_id) else {
            return;
        };
        (
            clip.path.clone(),
            clip.path
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default(),
            format!(
                "{} / {}",
                clip.info.profile.video_codec,
                if clip.info.profile.audio_codec.is_empty() {
                    "—"
                } else {
                    &clip.info.profile.audio_codec
                }
            ),
            format!(
                "{}x{}",
                clip.info.profile.resolution.width, clip.info.profile.resolution.height
            ),
            crate::view_model::timeline_vm::format_duration(clip.info.duration),
            format!("{:.2}", clip.info.profile.frame_rate_mhz as f64 / 1000.0),
            clip.trim_in_us(),
            clip.trim_out_us(),
        )
    };

    {
        let mut pv = preview.lock().expect("preview mutex poisoned");
        pv.clip_id = Some(clip_id);
        pv.playhead_us = 0;
        pv.duration_us = 0;
        pv.playing = false;
        pv.pending_seek_us = initial_seek_us;
    }

    if let Some(window) = weak.upgrade() {
        window.set_selected_id(SharedString::from(clip_id.to_string()));
        window.set_has_selection(true);
        window.set_selected_name(SharedString::from(name));
        window.set_selected_codec(SharedString::from(codec));
        window.set_selected_resolution(SharedString::from(resolution));
        window.set_selected_duration(SharedString::from(duration));
        window.set_selected_fps(SharedString::from(fps));
        window.set_playhead_text(SharedString::from("0:00"));
        window.set_playing(false);
    }

    if let Err(err) = cmd_tx.try_send(Command::OpenPreview {
        clip_id,
        path,
        trim_in_us,
        trim_out_us,
    }) {
        tracing::warn!(error = %err, "failed to enqueue OpenPreview");
    }
}

fn on_play_toggle(
    weak: Weak<AppWindow>,
    cmd_tx: mpsc::Sender<Command>,
    preview: SharedPreview,
) {
    let should_play = {
        let mut pv = preview.lock().expect("preview mutex poisoned");
        if pv.clip_id.is_none() {
            return;
        }
        pv.playing = !pv.playing;
        pv.playing
    };

    let cmd = if should_play {
        Command::PlayPreview
    } else {
        Command::PausePreview
    };
    if let Err(err) = cmd_tx.try_send(cmd) {
        tracing::warn!(error = %err, "failed to enqueue play/pause");
    }
    if let Some(window) = weak.upgrade() {
        window.set_playing(should_play);
    }
}

/// Map a click on the ruler to "select that clip and seek inside it".
/// Same-clip seek → SeekPreview; cross-clip seek → select + pending seek.
fn on_seek_fraction(
    weak: Weak<AppWindow>,
    cmd_tx: mpsc::Sender<Command>,
    playlist: SharedPlaylist,
    preview: SharedPreview,
    fraction: f32,
) {
    let target = {
        let pl = playlist.lock().expect("playlist mutex poisoned");
        timeline_view::fraction_to_clip_local(&pl, fraction)
    };
    let Some((target_clip, local_us)) = target else {
        return;
    };

    let current = preview.lock().expect("preview mutex poisoned").clip_id;
    if current == Some(target_clip) {
        if let Err(err) = cmd_tx.try_send(Command::SeekPreview { pts_us: local_us }) {
            tracing::warn!(error = %err, "seek failed");
        }
    } else {
        select_and_open(weak, cmd_tx, playlist, preview, target_clip, Some(local_us));
    }
}

fn on_zoom(
    weak: Weak<AppWindow>,
    playlist: SharedPlaylist,
    zoom: SharedZoom,
    factor: f32,
) {
    let new_zoom = {
        let mut z = zoom.lock().expect("zoom mutex poisoned");
        *z = (*z * factor).clamp(ZOOM_MIN, ZOOM_MAX);
        *z
    };
    if let Some(window) = weak.upgrade() {
        let pl = playlist.lock().expect("playlist mutex poisoned");
        timeline_view::refresh_ruler(&window, &pl, new_zoom);
    }
}

fn on_jump(cmd_tx: mpsc::Sender<Command>, preview: SharedPreview, target: JumpTo) {
    let pts_us = {
        let pv = preview.lock().expect("preview mutex poisoned");
        if pv.clip_id.is_none() {
            return;
        }
        match target {
            JumpTo::Start => 0,
            JumpTo::End => (pv.duration_us - 100_000).max(0), // 100ms before end
        }
    };
    if let Err(err) = cmd_tx.try_send(Command::SeekPreview { pts_us }) {
        tracing::warn!(error = %err, "jump seek failed");
    }
}

fn on_export(
    weak: Weak<AppWindow>,
    cmd_tx: mpsc::Sender<Command>,
    playlist: SharedPlaylist,
    active_job: ActiveJob,
    job_meta: JobMetaMap,
    config: SharedConfig,
) {
    let starting_dir = config
        .lock()
        .expect("config mutex poisoned")
        .default_output_dir
        .clone();

    let output_path = match pick_output_path(starting_dir.as_deref()) {
        Some(p) => p,
        None => return,
    };

    let plan_result = {
        let pl = playlist.lock().expect("playlist mutex poisoned");
        if pl.is_empty() {
            if let Some(window) = weak.upgrade() {
                window.set_status_text("Add some clips first".into());
            }
            return;
        }
        let spec = ExportSpec {
            output_path: output_path.clone(),
            format: ContainerFormat::Mp4,
            quality: Quality::Lossless,
        };
        MergePlanner::build(&pl, spec)
    };

    let plan = match plan_result {
        Ok(p) => p,
        Err(err) => {
            tracing::warn!(error = %err, "merge plan build failed");
            if let Some(window) = weak.upgrade() {
                window.set_status_text(format!("Plan error: {err}").into());
            }
            return;
        }
    };

    let id = Uuid::new_v4();
    job_meta.lock().expect("job_meta mutex poisoned").insert(
        id,
        JobMeta {
            output_path: output_path.clone(),
            input_count: plan.inputs.len(),
            duration: plan.total_duration,
        },
    );
    *active_job.lock().expect("active_job mutex poisoned") = Some(id);

    if let Some(window) = weak.upgrade() {
        window.set_exporting(true);
        window.set_export_progress(0.0);
        window.set_status_text("Exporting…".into());
    }

    if let Err(err) = cmd_tx.try_send(Command::Merge { id, plan }) {
        tracing::warn!(error = %err, "failed to enqueue merge command");
        job_meta.lock().expect("job_meta mutex poisoned").remove(&id);
        *active_job.lock().expect("active_job mutex poisoned") = None;
        if let Some(window) = weak.upgrade() {
            window.set_exporting(false);
            window.set_status_text("Failed to enqueue export".into());
        }
    }
}

fn on_cancel(cmd_tx: mpsc::Sender<Command>, active_job: ActiveJob) {
    let id = match *active_job.lock().expect("active_job mutex poisoned") {
        Some(id) => id,
        None => return,
    };
    if let Err(err) = cmd_tx.try_send(Command::Cancel { id }) {
        tracing::warn!(error = %err, "failed to enqueue cancel");
    }
}

fn pick_output_path(starting_dir: Option<&std::path::Path>) -> Option<PathBuf> {
    let mut dialog = rfd::FileDialog::new()
        .add_filter("MP4 video", &["mp4"])
        .set_title("Save merged video")
        .set_file_name("merged.mp4");
    if let Some(dir) = starting_dir {
        dialog = dialog.set_directory(dir);
    }
    dialog.save_file()
}

