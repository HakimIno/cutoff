use std::path::PathBuf;
use std::time::Duration;

use slint::{ComponentHandle, SharedString, Weak};
use tokio::sync::mpsc;
use uuid::Uuid;
use video_merger_core::domain::{
    Clip, ContainerFormat, ExportSpec, Playlist, Quality, TransformChannel,
};
use video_merger_core::services::{self, TrimSide};
use video_merger_persistence::Project;
use video_merger_ui::AppWindow;
use video_merger_worker::Command;

use super::{models, timeline_view};
use super::{
    ActiveJob, BridgeState, JobMeta, JobMetaMap, SharedConfig, SharedPlaylist, SharedPreview,
    SharedUndo, SharedZoom, ZOOM_DEFAULT, ZOOM_MAX, ZOOM_MIN,
};

const DROP_TARGET_NONE: i32 = -1;
const DRAG_ID_NONE: &str = "";

/// Snap tolerance for "is there a keyframe at the playhead": ~20ms, roughly a
/// frame, so a click lands on an existing key instead of stacking a new one.
const KEY_TOL_US: i64 = 20_000;

pub fn install(window: &AppWindow, cmd_tx: mpsc::Sender<Command>, state: BridgeState) {
    {
        let tx = cmd_tx.clone();
        let weak = window.as_weak();
        window.on_browse_clicked(move || on_browse(weak.clone(), tx.clone()));
    }
    {
        let tx = cmd_tx.clone();
        let weak = window.as_weak();
        let pl = state.playlist.clone();
        let zoom = state.zoom.clone();
        let undo = state.undo.clone();
        let tracks = state.tracks.clone();
        let proj = state.project.clone();
        window.on_remove_clicked(move |id| {
            on_remove(weak.clone(), tx.clone(), pl.clone(), tracks.clone(), proj.clone(), zoom.clone(), undo.clone(), id)
        });
    }
    {
        let weak = window.as_weak();
        window.on_drag_started(move |id| on_drag_started(weak.clone(), id));
    }
    {
        let weak = window.as_weak();
        let pl = state.playlist.clone();
        let pv = state.preview.clone();
        let proj = state.project.clone();
        let zoom = state.zoom.clone();
        window.on_drag_moved(move |id, dx, dy| {
            on_drag_moved(weak.clone(), pl.clone(), pv.clone(), proj.clone(), zoom.clone(), id, dx, dy)
        });
    }
    {
        let tx = cmd_tx.clone();
        let weak = window.as_weak();
        let pl = state.playlist.clone();
        let pv = state.preview.clone();
        let zoom = state.zoom.clone();
        let undo = state.undo.clone();
        let tracks = state.tracks.clone();
        let proj = state.project.clone();
        window.on_drag_released(move |id, dx, dy| {
            on_drag_released(
                weak.clone(),
                tx.clone(),
                pl.clone(),
                pv.clone(),
                tracks.clone(),
                proj.clone(),
                zoom.clone(),
                undo.clone(),
                id,
                dx,
                dy,
            )
        });
    }
    {
        let tx = cmd_tx.clone();
        let weak = window.as_weak();
        let pl = state.playlist.clone();
        let pv = state.preview.clone();
        let tracks = state.tracks.clone();
        let proj = state.project.clone();
        window.on_clip_selected(move |id| {
            let Ok(uuid) = Uuid::parse_str(id.as_str()) else { return };
            select_and_open(weak.clone(), tx.clone(), pl.clone(), pv.clone(), &tracks, &proj, uuid, None);
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
        let proj = state.project.clone();
        window.on_seek_fraction(move |f| {
            on_seek_fraction(weak.clone(), tx.clone(), pl.clone(), pv.clone(), proj.clone(), f)
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
        let weak = window.as_weak();
        let pl = state.playlist.clone();
        let zoom = state.zoom.clone();
        window.on_set_zoom(move |px_per_sec| on_set_zoom(weak.clone(), pl.clone(), zoom.clone(), px_per_sec));
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
        let pv = state.preview.clone();
        let zoom = state.zoom.clone();
        let undo = state.undo.clone();
        let tracks = state.tracks.clone();
        let proj = state.project.clone();
        window.on_trim_released(move |id, is_right, dx_px| {
            on_trim_released(
                weak.clone(),
                tx.clone(),
                pl.clone(),
                pv.clone(),
                tracks.clone(),
                proj.clone(),
                zoom.clone(),
                undo.clone(),
                id,
                is_right,
                dx_px,
            )
        });
    }
    {
        let tx = cmd_tx.clone();
        let weak = window.as_weak();
        let pl = state.playlist.clone();
        let pv = state.preview.clone();
        let zoom = state.zoom.clone();
        let undo = state.undo.clone();
        let tracks = state.tracks.clone();
        let proj = state.project.clone();
        window.on_split(move || {
            on_split(
                weak.clone(),
                tx.clone(),
                pl.clone(),
                pv.clone(),
                tracks.clone(),
                proj.clone(),
                zoom.clone(),
                undo.clone(),
            )
        });
    }
    {
        let tx = cmd_tx.clone();
        let weak = window.as_weak();
        let pl = state.playlist.clone();
        let pv = state.preview.clone();
        let zoom = state.zoom.clone();
        let undo = state.undo.clone();
        let tracks = state.tracks.clone();
        let proj = state.project.clone();
        window.on_ripple_delete(move || {
            on_ripple_delete(weak.clone(), tx.clone(), pl.clone(), pv.clone(), tracks.clone(), proj.clone(), zoom.clone(), undo.clone())
        });
    }
    {
        let tx = cmd_tx.clone();
        let pl = state.playlist.clone();
        let pv = state.preview.clone();
        window.on_step_frame(move |delta| {
            on_step_frame(tx.clone(), pl.clone(), pv.clone(), delta)
        });
    }
    {
        let tx = cmd_tx.clone();
        let weak = window.as_weak();
        let pl = state.playlist.clone();
        let pv = state.preview.clone();
        let zoom = state.zoom.clone();
        let undo = state.undo.clone();
        let tracks = state.tracks.clone();
        let proj = state.project.clone();
        window.on_set_in(move || {
            on_set_inout(
                weak.clone(),
                tx.clone(),
                pl.clone(),
                pv.clone(),
                tracks.clone(),
                proj.clone(),
                zoom.clone(),
                undo.clone(),
                TrimSide::Left,
            )
        });
    }
    {
        let tx = cmd_tx.clone();
        let weak = window.as_weak();
        let pl = state.playlist.clone();
        let pv = state.preview.clone();
        let zoom = state.zoom.clone();
        let undo = state.undo.clone();
        let tracks = state.tracks.clone();
        let proj = state.project.clone();
        window.on_set_out(move || {
            on_set_inout(
                weak.clone(),
                tx.clone(),
                pl.clone(),
                pv.clone(),
                tracks.clone(),
                proj.clone(),
                zoom.clone(),
                undo.clone(),
                TrimSide::Right,
            )
        });
    }
    {
        let tx = cmd_tx.clone();
        let weak = window.as_weak();
        let pl = state.playlist.clone();
        let pv = state.preview.clone();
        let zoom = state.zoom.clone();
        let undo = state.undo.clone();
        let tracks = state.tracks.clone();
        let proj = state.project.clone();
        window.on_undo(move || on_undo(weak.clone(), tx.clone(), pl.clone(), pv.clone(), tracks.clone(), proj.clone(), zoom.clone(), undo.clone()));
    }
    {
        let tx = cmd_tx.clone();
        let weak = window.as_weak();
        let pl = state.playlist.clone();
        let pv = state.preview.clone();
        let zoom = state.zoom.clone();
        let undo = state.undo.clone();
        let tracks = state.tracks.clone();
        let proj = state.project.clone();
        window.on_redo(move || on_redo(weak.clone(), tx.clone(), pl.clone(), pv.clone(), tracks.clone(), proj.clone(), zoom.clone(), undo.clone()));
    }
    {
        let weak = window.as_weak();
        let pl = state.playlist.clone();
        let proj = state.project.clone();
        let tracks = state.tracks.clone();
        let zoom = state.zoom.clone();
        window.on_save_project(move || on_save_project(weak.clone(), pl.clone(), proj.clone(), tracks.clone(), zoom.clone()));
    }
    {
        let tx = cmd_tx.clone();
        let weak = window.as_weak();
        let pl = state.playlist.clone();
        let proj = state.project.clone();
        let tracks = state.tracks.clone();
        let pv = state.preview.clone();
        let zoom = state.zoom.clone();
        let undo = state.undo.clone();
        window.on_load_project(move || {
            on_load_project(
                weak.clone(),
                tx.clone(),
                pl.clone(),
                proj.clone(),
                tracks.clone(),
                pv.clone(),
                zoom.clone(),
                undo.clone(),
            )
        });
    }
    {
        let weak = window.as_weak();
        window.on_dismiss_toast(move || {
            if let Some(w) = weak.upgrade() {
                w.set_toast_visible(false);
            }
        });
    }
    {
        let pl = state.playlist.clone();
        window.on_reveal_in_finder(move |id| on_reveal_in_finder(pl.clone(), id));
    }
    {
        let tx = cmd_tx.clone();
        let weak = window.as_weak();
        let audio = state.audio.clone();
        window.on_toggle_master_mute(move || on_toggle_master_mute(weak.clone(), tx.clone(), audio.clone()));
    }
    {
        let tx = cmd_tx.clone();
        let weak = window.as_weak();
        let pl = state.playlist.clone();
        let zoom = state.zoom.clone();
        let undo = state.undo.clone();
        let tracks = state.tracks.clone();
        let proj = state.project.clone();
        window.on_move_to_track(move |id, t| {
            on_move_to_track(weak.clone(), tx.clone(), pl.clone(), tracks.clone(), proj.clone(), zoom.clone(), undo.clone(), id, t as u8)
        });
    }
    {
        let tx = cmd_tx.clone();
        let weak = window.as_weak();
        let pl = state.playlist.clone();
        let zoom = state.zoom.clone();
        let undo = state.undo.clone();
        let tracks = state.tracks.clone();
        let proj = state.project.clone();
        window.on_detach_audio(move |id| {
            on_detach_audio(weak.clone(), tx.clone(), pl.clone(), tracks.clone(), proj.clone(), zoom.clone(), undo.clone(), id)
        });
    }
    {
        let tx = cmd_tx.clone();
        let weak = window.as_weak();
        let pl = state.playlist.clone();
        let tracks = state.tracks.clone();
        let proj = state.project.clone();
        window.on_toggle_track_mute(move |idx| on_toggle_track_mute(weak.clone(), tx.clone(), pl.clone(), tracks.clone(), proj.clone(), idx));
    }
    {
        let tx = cmd_tx.clone();
        let weak = window.as_weak();
        let pl = state.playlist.clone();
        let tracks = state.tracks.clone();
        let proj = state.project.clone();
        window.on_toggle_track_solo(move |idx| on_toggle_track_solo(weak.clone(), tx.clone(), pl.clone(), tracks.clone(), proj.clone(), idx));
    }
    {
        let tx = cmd_tx.clone();
        let weak = window.as_weak();
        let pl = state.playlist.clone();
        let tracks = state.tracks.clone();
        let proj = state.project.clone();
        window.on_toggle_track_lock(move |idx| on_toggle_track_lock(weak.clone(), tx.clone(), pl.clone(), tracks.clone(), proj.clone(), idx));
    }
    {
        let tx = cmd_tx.clone();
        let weak = window.as_weak();
        let pl = state.playlist.clone();
        let tracks = state.tracks.clone();
        let proj = state.project.clone();
        window.on_add_video_track(move || {
            on_add_track(weak.clone(), tx.clone(), pl.clone(), tracks.clone(), proj.clone(), video_merger_core::domain::TrackKind::Video);
        });
    }
    {
        let tx = cmd_tx.clone();
        let weak = window.as_weak();
        let pl = state.playlist.clone();
        let tracks = state.tracks.clone();
        let proj = state.project.clone();
        window.on_add_audio_track(move || {
            on_add_track(weak.clone(), tx.clone(), pl.clone(), tracks.clone(), proj.clone(), video_merger_core::domain::TrackKind::Audio);
        });
    }
    {
        let tx = cmd_tx.clone();
        let weak = window.as_weak();
        let pl = state.playlist.clone();
        let tracks = state.tracks.clone();
        let proj = state.project.clone();
        let undo = state.undo.clone();
        let zoom = state.zoom.clone();
        window.on_delete_track(move |idx| {
            on_delete_track(weak.clone(), tx.clone(), pl.clone(), tracks.clone(), proj.clone(), undo.clone(), zoom.clone(), idx);
        });
    }
    {
        let tx = cmd_tx.clone();
        let weak = window.as_weak();
        let pl = state.playlist.clone();
        let tracks = state.tracks.clone();
        let proj = state.project.clone();
        let zoom = state.zoom.clone();
        let undo = state.undo.clone();
        window.on_selected_clip_volume_changed(move |vol| {
            on_selected_clip_volume_changed(weak.clone(), tx.clone(), pl.clone(), tracks.clone(), proj.clone(), zoom.clone(), undo.clone(), vol);
        });
    }
    {
        let tx = cmd_tx.clone();
        let weak = window.as_weak();
        let pl = state.playlist.clone();
        let tracks = state.tracks.clone();
        let proj = state.project.clone();
        let zoom = state.zoom.clone();
        let undo = state.undo.clone();
        window.on_selected_clip_muted_changed(move |muted| {
            on_selected_clip_muted_changed(weak.clone(), tx.clone(), pl.clone(), tracks.clone(), proj.clone(), zoom.clone(), undo.clone(), muted);
        });
    }
    {
        let tx = cmd_tx.clone();
        let weak = window.as_weak();
        let pl = state.playlist.clone();
        let tracks = state.tracks.clone();
        let proj = state.project.clone();
        let undo = state.undo.clone();
        window.on_selected_clip_opacity_changed(move |opacity| {
            on_selected_clip_opacity_changed(weak.clone(), tx.clone(), pl.clone(), tracks.clone(), proj.clone(), undo.clone(), opacity);
        });
    }
    {
        let tx = cmd_tx.clone();
        let weak = window.as_weak();
        let pl = state.playlist.clone();
        let tracks = state.tracks.clone();
        let proj = state.project.clone();
        let undo = state.undo.clone();
        window.on_selected_clip_scale_changed(move |scale| {
            on_selected_clip_scale_changed(weak.clone(), tx.clone(), pl.clone(), tracks.clone(), proj.clone(), undo.clone(), scale);
        });
    }
    {
        let tx = cmd_tx.clone();
        let weak = window.as_weak();
        let pl = state.playlist.clone();
        let tracks = state.tracks.clone();
        let proj = state.project.clone();
        let undo = state.undo.clone();
        window.on_selected_clip_position_x_changed(move |x| {
            on_selected_clip_position_x_changed(weak.clone(), tx.clone(), pl.clone(), tracks.clone(), proj.clone(), undo.clone(), x);
        });
    }
    {
        let tx = cmd_tx.clone();
        let weak = window.as_weak();
        let pl = state.playlist.clone();
        let tracks = state.tracks.clone();
        let proj = state.project.clone();
        let undo = state.undo.clone();
        window.on_selected_clip_position_y_changed(move |y| {
            on_selected_clip_position_y_changed(weak.clone(), tx.clone(), pl.clone(), tracks.clone(), proj.clone(), undo.clone(), y);
        });
    }
    {
        let tx = cmd_tx.clone();
        let weak = window.as_weak();
        let pl = state.playlist.clone();
        let tracks = state.tracks.clone();
        let proj = state.project.clone();
        window.on_selected_clip_scale_x_changed(move |s| {
            on_selected_clip_transform_f32(weak.clone(), tx.clone(), pl.clone(), tracks.clone(), proj.clone(), s, |c, v| c.scale_x = v);
        });
    }
    {
        let tx = cmd_tx.clone();
        let weak = window.as_weak();
        let pl = state.playlist.clone();
        let tracks = state.tracks.clone();
        let proj = state.project.clone();
        window.on_selected_clip_scale_y_changed(move |s| {
            on_selected_clip_transform_f32(weak.clone(), tx.clone(), pl.clone(), tracks.clone(), proj.clone(), s, |c, v| c.scale_y = v);
        });
    }
    {
        let tx = cmd_tx.clone();
        let weak = window.as_weak();
        let pl = state.playlist.clone();
        let tracks = state.tracks.clone();
        let proj = state.project.clone();
        window.on_selected_clip_rotation_changed(move |r| {
            on_selected_clip_transform_f32(weak.clone(), tx.clone(), pl.clone(), tracks.clone(), proj.clone(), r, |c, v| c.rotation_deg = v);
        });
    }
    {
        let tx = cmd_tx.clone();
        let weak = window.as_weak();
        let pl = state.playlist.clone();
        let tracks = state.tracks.clone();
        let proj = state.project.clone();
        window.on_selected_clip_flip_h_changed(move |m| {
            on_selected_clip_transform_bool(weak.clone(), tx.clone(), pl.clone(), tracks.clone(), proj.clone(), m, |c, v| c.flip_h = v);
        });
    }
    {
        let tx = cmd_tx.clone();
        let weak = window.as_weak();
        let pl = state.playlist.clone();
        let tracks = state.tracks.clone();
        let proj = state.project.clone();
        window.on_selected_clip_flip_v_changed(move |m| {
            on_selected_clip_transform_bool(weak.clone(), tx.clone(), pl.clone(), tracks.clone(), proj.clone(), m, |c, v| c.flip_v = v);
        });
    }
    {
        let tx = cmd_tx.clone();
        let weak = window.as_weak();
        let pl = state.playlist.clone();
        let tracks = state.tracks.clone();
        let proj = state.project.clone();
        window.on_selected_clip_brightness_changed(move |b| {
            on_selected_clip_transform_f32(weak.clone(), tx.clone(), pl.clone(), tracks.clone(), proj.clone(), b, |c, v| c.brightness = v);
        });
    }
    {
        let tx = cmd_tx.clone();
        let weak = window.as_weak();
        let pl = state.playlist.clone();
        let tracks = state.tracks.clone();
        let proj = state.project.clone();
        window.on_selected_clip_contrast_changed(move |c| {
            on_selected_clip_transform_f32(weak.clone(), tx.clone(), pl.clone(), tracks.clone(), proj.clone(), c, |clip, v| clip.contrast = v);
        });
    }
    {
        let tx = cmd_tx.clone();
        let weak = window.as_weak();
        let pl = state.playlist.clone();
        let tracks = state.tracks.clone();
        let proj = state.project.clone();
        window.on_selected_clip_saturation_changed(move |s| {
            on_selected_clip_transform_f32(weak.clone(), tx.clone(), pl.clone(), tracks.clone(), proj.clone(), s, |c, v| c.saturation = v);
        });
    }
    {
        let tx = cmd_tx.clone();
        let weak = window.as_weak();
        let pl = state.playlist.clone();
        let tracks = state.tracks.clone();
        let proj = state.project.clone();
        window.on_selected_clip_color_grading_changed(move |name, val| {
            on_selected_clip_color_grading_changed(
                weak.clone(),
                tx.clone(),
                pl.clone(),
                tracks.clone(),
                proj.clone(),
                name.to_string(),
                val,
            );
        });
    }
    {
        let weak = window.as_weak();
        let pl = state.playlist.clone();
        let undo = state.undo.clone();
        window.on_selected_clip_transform_gesture_begin(move || {
            // Snapshot once at the start of a scrub/drag/toggle so the whole
            // gesture undoes in a single step (not per intermediate value).
            if weak.upgrade().is_some() {
                if let Ok(pl) = pl.lock() {
                    checkpoint(&undo, &pl);
                }
            }
        });
    }
    {
        let tx = cmd_tx.clone();
        let weak = window.as_weak();
        let pl = state.playlist.clone();
        let tracks = state.tracks.clone();
        let proj = state.project.clone();
        let pv = state.preview.clone();
        let undo = state.undo.clone();
        window.on_selected_clip_keyframe_toggle(move |ch| {
            on_selected_clip_keyframe_toggle(weak.clone(), tx.clone(), pl.clone(), tracks.clone(), proj.clone(), pv.clone(), undo.clone(), ch);
        });
    }
    {
        let tx = cmd_tx.clone();
        let weak = window.as_weak();
        let pl = state.playlist.clone();
        let tracks = state.tracks.clone();
        let proj = state.project.clone();
        let pv = state.preview.clone();
        let undo = state.undo.clone();
        window.on_selected_clip_keyframe_interp_changed(move |ch, mode| {
            on_selected_clip_keyframe_interp_changed(
                weak.clone(),
                tx.clone(),
                pl.clone(),
                tracks.clone(),
                proj.clone(),
                pv.clone(),
                undo.clone(),
                ch,
                mode.to_string(),
            );
        });
    }
    {
        let tx = cmd_tx.clone();
        let weak = window.as_weak();
        let pl = state.playlist.clone();
        let proj = state.project.clone();
        let tracks = state.tracks.clone();
        let aj = state.active_job.clone();
        let jm = state.job_meta.clone();
        let cfg = state.config.clone();
        window.on_export_clicked(move || {
            on_export(
                weak.clone(),
                tx.clone(),
                pl.clone(),
                proj.clone(),
                tracks.clone(),
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
    cmd_tx: mpsc::Sender<Command>,
    playlist: SharedPlaylist,
    tracks: super::SharedTracks,
    project: super::SharedProject,
    zoom: SharedZoom,
    undo: SharedUndo,
    id: SharedString,
) {
    let Ok(uuid) = Uuid::parse_str(id.as_str()) else {
        tracing::warn!(%id, "remove: invalid clip id");
        return;
    };

    {
        let mut pl = playlist.lock().expect("playlist mutex poisoned");
        checkpoint(&undo, &pl);
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
    if let Some(window) = weak.upgrade() {
        refresh_tracks_ui(&window, &playlist, &tracks, &project);
    }
    sync_preview(&playlist, &tracks, &project, &cmd_tx);
}

fn on_drag_started(weak: Weak<AppWindow>, id: SharedString) {
    if let Some(window) = weak.upgrade() {
        window.set_drag_from_id(id);
        window.set_drop_target_index(DROP_TARGET_NONE);
    }
}

/// Translate a vertical drag (`dy_px`) into a destination `video_track` value,
/// constrained to tracks of the same kind as the clip's current track (video
/// clips stay among video lanes, audio among audio). Returns the clip's current
/// track when the drag stays in-row or would cross kinds.
fn resolve_drag_track(project: &video_merger_core::domain::Project, clip_track: u8, dy_px: f32) -> u8 {
    let cur_idx = project.track_index_for_video_track_value(clip_track);
    let row_delta = (dy_px / timeline_view::LANE_ROW_PX).round() as i32;
    if row_delta == 0 || project.tracks.is_empty() {
        return clip_track;
    }
    let n = project.tracks.len() as i32;
    let target_idx = (cur_idx as i32 + row_delta).clamp(0, n - 1) as usize;
    if project.tracks.get(target_idx).map(|t| t.kind) != project.tracks.get(cur_idx).map(|t| t.kind) {
        return clip_track;
    }
    project.video_track_value_for_index(target_idx)
}

#[allow(clippy::too_many_arguments)]
fn on_drag_moved(
    weak: Weak<AppWindow>,
    playlist: SharedPlaylist,
    preview: SharedPreview,
    project: super::SharedProject,
    zoom: SharedZoom,
    id: SharedString,
    delta_px: f32,
    delta_y_px: f32,
) {
    use video_merger_core::domain::TrackKind;
    let Ok(uuid) = Uuid::parse_str(id.as_str()) else {
        return;
    };
    let z = *zoom.lock().expect("zoom mutex poisoned");
    let playhead = preview.lock().map(|p| p.playhead_us).unwrap_or(0);
    let pl = playlist.lock().expect("playlist mutex poisoned");
    let proj = project.lock().expect("project mutex poisoned");

    let Some((clip_track, dur_us)) = pl
        .clips()
        .iter()
        .find(|c| c.id == uuid)
        .map(|c| (c.video_track, c.effective_duration_us()))
    else {
        return;
    };
    let Some(cur_start) = timeline_view::effective_start_us(&pl, uuid) else {
        return;
    };
    let dest_track = resolve_drag_track(&proj, clip_track, delta_y_px);
    let Some(out) = timeline_view::drag_to_start_us(&pl, z, uuid, delta_px, playhead, dest_track) else {
        return;
    };

    // Geometry for the floating ghost (follows the cursor) and the snapped
    // drop-zone outline (where it will actually land).
    let cur_row = proj.track_index_for_video_track_value(clip_track);
    let dest_row = proj.track_index_for_video_track_value(dest_track);
    let is_audio = matches!(proj.tracks.get(cur_row).map(|t| t.kind), Some(TrackKind::Audio));

    let cur_x = cur_start as f32 / 1_000_000.0 * z;
    let width = (dur_us as f32 / 1_000_000.0 * z).max(44.0);
    let ghost_x = cur_x + delta_px;
    let ghost_y = timeline_view::RULER_OFFSET_PX
        + cur_row as f32 * timeline_view::LANE_ROW_PX
        + timeline_view::CARD_Y_PX
        + delta_y_px;
    let drop_zone_y =
        timeline_view::RULER_OFFSET_PX + dest_row as f32 * timeline_view::LANE_ROW_PX;

    if let Some(window) = weak.upgrade() {
        window.set_drop_target_index(0);
        window.set_drop_indicator_x(out.indicator_x_px);
        window.set_drop_zone_y(drop_zone_y);
        window.set_drag_ghost_x(ghost_x);
        window.set_drag_ghost_y(ghost_y);
        window.set_drag_ghost_w(width);
        window.set_drag_ghost_audio(is_audio);
    }
}

#[allow(clippy::too_many_arguments)]
fn on_drag_released(
    weak: Weak<AppWindow>,
    cmd_tx: mpsc::Sender<Command>,
    playlist: SharedPlaylist,
    preview: SharedPreview,
    tracks: super::SharedTracks,
    project: super::SharedProject,
    zoom: SharedZoom,
    undo: SharedUndo,
    id: SharedString,
    delta_px: f32,
    delta_y_px: f32,
) {
    let Some(window) = weak.upgrade() else { return };
    window.set_drag_from_id(SharedString::from(DRAG_ID_NONE));
    window.set_drop_target_index(DROP_TARGET_NONE);

    let Ok(uuid) = Uuid::parse_str(id.as_str()) else {
        return;
    };
    let z = *zoom.lock().expect("zoom mutex poisoned");
    let playhead = preview.lock().map(|p| p.playhead_us).unwrap_or(0);

    // Destination track (from the vertical drag) + snapped start; bail if
    // nothing actually moved.
    let resolved = {
        let pl = playlist.lock().expect("playlist mutex poisoned");
        let proj = project.lock().expect("project mutex poisoned");
        let Some(cur_track) = pl.clips().iter().find(|c| c.id == uuid).map(|c| c.video_track) else {
            return;
        };
        let dest_track = resolve_drag_track(&proj, cur_track, delta_y_px);
        let Some(out) = timeline_view::drag_to_start_us(&pl, z, uuid, delta_px, playhead, dest_track) else {
            return;
        };
        let unchanged = dest_track == cur_track
            && timeline_view::effective_start_us(&pl, uuid) == Some(out.new_start_us);
        if unchanged {
            return;
        }
        (dest_track, out.new_start_us)
    };
    let (dest_track, new_start) = resolved;

    {
        let mut pl = playlist.lock().expect("playlist mutex poisoned");
        checkpoint(&undo, &pl);
        if let Some(clip) = pl.clips_mut().iter_mut().find(|c| c.id == uuid) {
            clip.video_track = dest_track;
            clip.start_us = Some(new_start);
        }
    }

    refresh_tracks_ui(&window, &playlist, &tracks, &project);
    {
        let pl = playlist.lock().expect("playlist mutex poisoned");
        models::sync_clips(&window, &pl);
        timeline_view::refresh_ruler(&window, &pl, z);
    }
    sync_preview(&playlist, &tracks, &project, &cmd_tx);
}

/// Refresh the multi-track project from the (mutable) playlist + track-state,
/// stash it in `project`, and submit it to the preview worker.
fn sync_preview(
    playlist: &SharedPlaylist,
    tracks: &super::SharedTracks,
    project: &super::SharedProject,
    cmd_tx: &mpsc::Sender<Command>,
) {
    let pl = playlist.lock().expect("playlist mutex poisoned");
    let ts = tracks.lock().expect("tracks mutex poisoned");
    let mut proj = project.lock().expect("project mutex poisoned");
    proj.populate_from_playlist(&pl);
    for (idx, track) in proj.tracks.iter_mut().enumerate() {
        track.muted = ts.muted.get(idx).copied().unwrap_or(false);
        track.solo = ts.soloed.get(idx).copied().unwrap_or(false);
        track.locked = ts.locked.get(idx).copied().unwrap_or(false);
    }

    // Throttle preview updates to 30 FPS (~33ms) to avoid queue buildup during dragging.
    // Ensure the final frame is rendered by spawning a delayed fallback task.
    static LAST_SYNC: std::sync::OnceLock<std::sync::Mutex<std::time::Instant>> = std::sync::OnceLock::new();
    let now = std::time::Instant::now();
    let mut last_sync = LAST_SYNC.get_or_init(|| std::sync::Mutex::new(now - std::time::Duration::from_millis(100))).lock().unwrap();
    
    if now.duration_since(*last_sync) >= std::time::Duration::from_millis(33) {
        *last_sync = now;
        let _ = cmd_tx.try_send(Command::OpenPreview { project: proj.clone() });
    } else {
        let tx = cmd_tx.clone();
        let proj_clone = proj.clone();
        thread_local! {
            static PREVIEW_TIMER: std::cell::RefCell<Option<slint::Timer>> = std::cell::RefCell::new(None);
        }
        PREVIEW_TIMER.with(|cell| {
            let mut timer = cell.borrow_mut();
            if timer.is_none() {
                *timer = Some(slint::Timer::default());
            }
            if let Some(timer) = timer.as_mut() {
                timer.start(
                    slint::TimerMode::SingleShot,
                    std::time::Duration::from_millis(50),
                    move || {
                        if let Some(cell) = LAST_SYNC.get() {
                            if let Ok(mut last_sync) = cell.lock() {
                                if std::time::Instant::now().duration_since(*last_sync) >= std::time::Duration::from_millis(45) {
                                    *last_sync = std::time::Instant::now();
                                    let _ = tx.try_send(Command::OpenPreview { project: proj_clone.clone() });
                                }
                            }
                        }
                    },
                );
            }
        });
    }
}

/// Rebuild the UI tracks model (per-track lanes + flags) from the current
/// state. Call after any structural change (clip add/remove/reorder/move,
/// track add/remove, track flag toggle, etc.).
fn refresh_tracks_ui(
    window: &AppWindow,
    playlist: &SharedPlaylist,
    tracks: &super::SharedTracks,
    project: &super::SharedProject,
) {
    let pl = playlist.lock().expect("playlist mutex poisoned");
    let ts = tracks.lock().expect("tracks mutex poisoned");
    let mut proj = project.lock().expect("project mutex poisoned");
    proj.populate_from_playlist(&pl);
    models::sync_tracks(window, &proj, &ts);
}

/// Select `clip_id`, push its metadata to the UI, and open a preview session.
/// `initial_seek_us` is stored as a pending seek so the first
/// `PreviewOpened` event triggers a `SeekPreview` automatically.
fn select_and_open(
    weak: Weak<AppWindow>,
    cmd_tx: mpsc::Sender<Command>,
    playlist: SharedPlaylist,
    preview: SharedPreview,
    tracks: &super::SharedTracks,
    project: &super::SharedProject,
    clip_id: Uuid,
    initial_seek_us: Option<i64>,
) {
    let (_path, name, codec, resolution, duration, fps, _trim_in_us, _trim_out_us, volume, muted, opacity, scale, position_x, position_y, scale_x, scale_y, rotation_deg, flip_h, flip_v, brightness, contrast, saturation, color_grading) = {
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
            clip.volume,
            clip.muted,
            clip.opacity,
            clip.scale,
            clip.position_x,
            clip.position_y,
            clip.scale_x,
            clip.scale_y,
            clip.rotation_deg,
            clip.flip_h,
            clip.flip_v,
            clip.brightness,
            clip.contrast,
            clip.saturation,
            clip.color_grading,
        )
    };

    let offset_us = {
        let pl = playlist.lock().expect("playlist mutex poisoned");
        timeline_view::selected_clip_window(&pl, Some(clip_id))
            .map(|(off, _)| off)
            .unwrap_or(0)
    };
    let target_seek_us = offset_us + initial_seek_us.unwrap_or(0);

    {
        let mut pv = preview.lock().expect("preview mutex poisoned");
        pv.clip_id = Some(clip_id);
        pv.playhead_us = target_seek_us;
        pv.playing = false;
        pv.frame_history.clear();
    }

    let (proj_w, proj_h, clip_w, clip_h) = {
        let pl = playlist.lock().expect("playlist mutex poisoned");
        let proj_res = pl.clips().first()
            .map(|c| (c.info.profile.resolution.width as i32, c.info.profile.resolution.height as i32))
            .unwrap_or((1920, 1080));
        let clip_res = pl.clips().iter().find(|c| c.id == clip_id)
            .map(|c| (c.info.profile.resolution.width as i32, c.info.profile.resolution.height as i32))
            .unwrap_or((1920, 1080));
        (proj_res.0, proj_res.1, clip_res.0, clip_res.1)
    };

    if let Some(window) = weak.upgrade() {
        window.set_project_width(proj_w);
        window.set_project_height(proj_h);
        window.set_selected_width(clip_w);
        window.set_selected_height(clip_h);
        window.set_selected_id(SharedString::from(clip_id.to_string()));
        window.set_has_selection(true);
        window.set_selected_name(SharedString::from(name));
        window.set_selected_codec(SharedString::from(codec));
        window.set_selected_resolution(SharedString::from(resolution));
        window.set_selected_duration(SharedString::from(duration));
        window.set_selected_fps(SharedString::from(fps));
        window.set_selected_volume(volume);
        window.set_selected_muted(muted);
        window.set_selected_opacity(opacity);
        window.set_selected_scale(scale);
        window.set_selected_position_x(position_x);
        window.set_selected_position_y(position_y);
        window.set_selected_scale_x(scale_x);
        window.set_selected_scale_y(scale_y);
        window.set_selected_rotation(rotation_deg);
        window.set_selected_flip_h(flip_h);
        window.set_selected_flip_v(flip_v);
        window.set_selected_brightness(brightness);
        window.set_selected_contrast(contrast);
        window.set_selected_saturation(saturation);
        window.set_selected_lift_r(color_grading.lift_r);
        window.set_selected_lift_g(color_grading.lift_g);
        window.set_selected_lift_b(color_grading.lift_b);
        window.set_selected_gamma_r(color_grading.gamma_r);
        window.set_selected_gamma_g(color_grading.gamma_g);
        window.set_selected_gamma_b(color_grading.gamma_b);
        window.set_selected_gain_r(color_grading.gain_r);
        window.set_selected_gain_g(color_grading.gain_g);
        window.set_selected_gain_b(color_grading.gain_b);
        window.set_selected_temperature(color_grading.temperature);
        window.set_selected_tint(color_grading.tint);
        window.set_selected_vignette(color_grading.vignette);
        window.set_playing(false);
    }

    if let Some(window) = weak.upgrade() {
        refresh_tracks_ui(&window, &playlist, tracks, project);
        refresh_keyframe_flags(&window, &playlist, project, &preview);
    }
    sync_preview(&playlist, tracks, project, &cmd_tx);

    let _ = cmd_tx.try_send(Command::SeekPreview { pts_us: target_seek_us });
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
        if !pv.playing {
            pv.frame_history.clear();
        }
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
        if !should_play {
            window.set_preview_fps(0.0);
        }
    }
}

/// Map a click on the ruler to "seek timeline playhead".
fn on_seek_fraction(
    weak: Weak<AppWindow>,
    cmd_tx: mpsc::Sender<Command>,
    playlist: SharedPlaylist,
    preview: SharedPreview,
    project: super::SharedProject,
    fraction: f32,
) {
    let total_us = {
        let pl = playlist.lock().expect("playlist mutex poisoned");
        timeline_view::total_duration_us(&pl)
    };
    let pts_us = (fraction as f64 * total_us as f64) as i64;
    {
        let mut pv = preview.lock().expect("preview mutex poisoned");
        pv.playhead_us = pts_us;
    }
    if let Err(err) = cmd_tx.try_send(Command::SeekPreview { pts_us }) {
        tracing::warn!(error = %err, "seek failed");
    }
    if let Some(window) = weak.upgrade() {
        window.set_playhead_fraction(fraction);
        window.set_playhead_text(SharedString::from(
            crate::view_model::timeline_vm::format_us(pts_us),
        ));
        // Update keyframe diamonds to reflect the new playhead position.
        refresh_keyframe_flags(&window, &playlist, &project, &preview);
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

/// Set the timeline zoom to an absolute `px_per_sec` value (from the zoom
/// slider), clamped to the supported range, and refresh the ruler.
fn on_set_zoom(
    weak: Weak<AppWindow>,
    playlist: SharedPlaylist,
    zoom: SharedZoom,
    px_per_sec: f32,
) {
    let new_zoom = {
        let mut z = zoom.lock().expect("zoom mutex poisoned");
        *z = px_per_sec.clamp(ZOOM_MIN, ZOOM_MAX);
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

fn on_trim_released(
    weak: Weak<AppWindow>,
    cmd_tx: mpsc::Sender<Command>,
    playlist: SharedPlaylist,
    preview: SharedPreview,
    tracks: super::SharedTracks,
    project: super::SharedProject,
    zoom: SharedZoom,
    undo: SharedUndo,
    id: SharedString,
    is_right: bool,
    delta_px: f32,
) {
    let Ok(uuid) = Uuid::parse_str(id.as_str()) else { return };
    if delta_px.abs() < 1.0 {
        return;
    }
    let zoom_val = *zoom.lock().expect("zoom mutex poisoned");
    if zoom_val <= 0.0 {
        return;
    }
    let delta_us = (delta_px as f64 / zoom_val as f64 * 1_000_000.0) as i64;

    let side = if is_right { TrimSide::Right } else { TrimSide::Left };
    let new_source_us = {
        let pl = playlist.lock().expect("playlist mutex poisoned");
        let Some(clip) = pl.clips().iter().find(|c| c.id == uuid) else {
            return;
        };
        if is_right {
            clip.trim_out_us() + delta_us
        } else {
            clip.trim_in_us() + delta_us
        }
    };

    {
        let mut pl = playlist.lock().expect("playlist mutex poisoned");
        checkpoint(&undo, &pl);
        if let Err(err) = services::trim(&mut pl, uuid, side, new_source_us) {
            tracing::warn!(error = %err, "trim failed");
            return;
        }
    }

    if let Some(window) = weak.upgrade() {
        let pl = playlist.lock().expect("playlist mutex poisoned");
        models::sync_clips(&window, &pl);
        timeline_view::refresh_ruler(&window, &pl, zoom_val);
    }
    // Re-open preview with the new bounds so playback respects the trim.
    select_and_open(weak, cmd_tx, playlist, preview, &tracks, &project, uuid, None);
}

fn on_split(
    weak: Weak<AppWindow>,
    cmd_tx: mpsc::Sender<Command>,
    playlist: SharedPlaylist,
    preview: SharedPreview,
    tracks: super::SharedTracks,
    project: super::SharedProject,
    zoom: SharedZoom,
    undo: SharedUndo,
) {
    // Split at the current (timeline-absolute) playhead. Resolve *which* clip
    // the playhead is over on the active track — this is what "Split at
    // playhead" means, and it works regardless of the clip's position because
    // `clip_at_us_on_track` returns the clip-local offset directly.
    let (track, abs_playhead) = {
        let pv = preview.lock().expect("preview mutex poisoned");
        let Some(sel) = pv.clip_id else { return };
        let abs = pv.playhead_us;
        let pl = playlist.lock().expect("playlist mutex poisoned");
        let Some(track) = pl.clips().iter().find(|c| c.id == sel).map(|c| c.video_track) else {
            return;
        };
        (track, abs)
    };
    let (clip_id, local_us) = {
        let pl = playlist.lock().expect("playlist mutex poisoned");
        match timeline_view::clip_at_us_on_track(&pl, track, abs_playhead) {
            Some(pair) => pair,
            None => return,
        }
    };
    if local_us <= 0 {
        return;
    }
    {
        let mut pl = playlist.lock().expect("playlist mutex poisoned");
        checkpoint(&undo, &pl);
        if let Err(err) = services::split_at(&mut pl, clip_id, local_us) {
            tracing::warn!(error = %err, "split failed");
            return;
        }
    }
    if let Some(window) = weak.upgrade() {
        let pl = playlist.lock().expect("playlist mutex poisoned");
        let z = *zoom.lock().expect("zoom mutex poisoned");
        models::sync_clips(&window, &pl);
        timeline_view::refresh_ruler(&window, &pl, z);
    }
    // Left half retains the original id; re-select it to reset preview bounds.
    select_and_open(weak, cmd_tx, playlist, preview, &tracks, &project, clip_id, None);
}

fn on_ripple_delete(
    weak: Weak<AppWindow>,
    cmd_tx: mpsc::Sender<Command>,
    playlist: SharedPlaylist,
    preview: SharedPreview,
    tracks: super::SharedTracks,
    project: super::SharedProject,
    zoom: SharedZoom,
    undo: SharedUndo,
) {
    let clip_id = {
        let mut pv = preview.lock().expect("preview mutex poisoned");
        let Some(id) = pv.clip_id else { return };
        pv.clip_id = None;
        pv.playhead_us = 0;
        pv.duration_us = 0;
        pv.playing = false;
        id
    };
    {
        let mut pl = playlist.lock().expect("playlist mutex poisoned");
        checkpoint(&undo, &pl);
        if let Err(err) = services::ripple_delete(&mut pl, clip_id) {
            tracing::warn!(error = %err, "ripple-delete failed");
            return;
        }
    }
    if let Some(window) = weak.upgrade() {
        {
            let pl = playlist.lock().expect("playlist mutex poisoned");
            let z = *zoom.lock().expect("zoom mutex poisoned");
            models::sync_clips(&window, &pl);
            timeline_view::refresh_ruler(&window, &pl, z);
            window.set_has_selection(false);
            window.set_selected_id(SharedString::from(""));
            window.set_playing(false);
            window.set_playhead_fraction(0.0);
            window.set_playhead_text(SharedString::from("0:00"));
            window.set_status_text(SharedString::from(format!(
                "{} clips in timeline",
                pl.len()
            )));
        }
        refresh_tracks_ui(&window, &playlist, &tracks, &project);
    }
    sync_preview(&playlist, &tracks, &project, &cmd_tx);
}

fn on_step_frame(
    cmd_tx: mpsc::Sender<Command>,
    playlist: SharedPlaylist,
    preview: SharedPreview,
    delta: i32,
) {
    let (clip_id, current_us, max_us) = {
        let pv = preview.lock().expect("preview mutex poisoned");
        let Some(id) = pv.clip_id else { return };
        (id, pv.playhead_us, pv.duration_us)
    };
    let frame_us = {
        let pl = playlist.lock().expect("playlist mutex poisoned");
        pl.clips()
            .iter()
            .find(|c| c.id == clip_id)
            .map(|c| {
                // 1_000_000 us/sec / (frame_rate_mhz / 1000) fps = 1e9 / mhz µs/frame
                let mhz = c.info.profile.frame_rate_mhz.max(1) as i64;
                1_000_000_000 / mhz
            })
            .unwrap_or(33_333)
    };
    let target = (current_us + (delta as i64) * frame_us).clamp(0, max_us.max(0));
    if let Err(err) = cmd_tx.try_send(Command::SeekPreview { pts_us: target }) {
        tracing::warn!(error = %err, "step-frame seek failed");
    }
}

fn on_set_inout(
    weak: Weak<AppWindow>,
    cmd_tx: mpsc::Sender<Command>,
    playlist: SharedPlaylist,
    preview: SharedPreview,
    tracks: super::SharedTracks,
    project: super::SharedProject,
    zoom: SharedZoom,
    undo: SharedUndo,
    side: TrimSide,
) {
    let (clip_id, current_local_us) = {
        let pv = preview.lock().expect("preview mutex poisoned");
        let Some(id) = pv.clip_id else { return };
        (id, pv.playhead_us)
    };
    let new_source_us = {
        let pl = playlist.lock().expect("playlist mutex poisoned");
        let Some(clip) = pl.clips().iter().find(|c| c.id == clip_id) else {
            return;
        };
        clip.trim_in_us() + current_local_us
    };
    {
        let mut pl = playlist.lock().expect("playlist mutex poisoned");
        checkpoint(&undo, &pl);
        if let Err(err) = services::trim(&mut pl, clip_id, side, new_source_us) {
            tracing::warn!(error = %err, "set-in/out failed");
            return;
        }
    }
    let z = *zoom.lock().expect("zoom mutex poisoned");
    if let Some(window) = weak.upgrade() {
        let pl = playlist.lock().expect("playlist mutex poisoned");
        models::sync_clips(&window, &pl);
        timeline_view::refresh_ruler(&window, &pl, z);
    }
    select_and_open(weak, cmd_tx, playlist, preview, &tracks, &project, clip_id, None);
}

/// Push the current playlist onto the undo stack before a mutation.
/// No-op if the stack mutex is poisoned (best-effort).
fn checkpoint(undo: &SharedUndo, playlist: &Playlist) {
    if let Ok(mut u) = undo.lock() {
        u.checkpoint(playlist);
    }
}

fn restore_playlist(
    weak: Weak<AppWindow>,
    cmd_tx: mpsc::Sender<Command>,
    playlist: SharedPlaylist,
    tracks: super::SharedTracks,
    project: super::SharedProject,
    preview: SharedPreview,
    zoom: SharedZoom,
    new_pl: Playlist,
) {
    {
        let mut pl = playlist.lock().expect("playlist mutex poisoned");
        *pl = new_pl;
    }
    // Selection may now point at a clip that no longer exists; clear it.
    {
        let mut pv = preview.lock().expect("preview mutex poisoned");
        pv.clip_id = None;
        pv.playhead_us = 0;
        pv.duration_us = 0;
        pv.playing = false;
        pv.pending_seek_us = None;
    }
    if let Some(window) = weak.upgrade() {
        {
            let pl = playlist.lock().expect("playlist mutex poisoned");
            let z = *zoom.lock().expect("zoom mutex poisoned");
            models::sync_clips(&window, &pl);
            timeline_view::refresh_ruler(&window, &pl, z);
            window.set_has_selection(false);
            window.set_selected_id(SharedString::from(""));
            window.set_playing(false);
            window.set_playhead_fraction(0.0);
            window.set_playhead_text(SharedString::from("0:00"));
        }
        refresh_tracks_ui(&window, &playlist, &tracks, &project);
    }
    sync_preview(&playlist, &tracks, &project, &cmd_tx);
}

fn on_undo(
    weak: Weak<AppWindow>,
    cmd_tx: mpsc::Sender<Command>,
    playlist: SharedPlaylist,
    preview: SharedPreview,
    tracks: super::SharedTracks,
    project: super::SharedProject,
    zoom: SharedZoom,
    undo: SharedUndo,
) {
    let prev = {
        let pl = playlist.lock().expect("playlist mutex poisoned");
        let mut u = undo.lock().expect("undo mutex poisoned");
        u.undo(&pl)
    };
    let Some(prev) = prev else { return };
    restore_playlist(weak, cmd_tx, playlist, tracks, project, preview, zoom, prev);
}

fn on_redo(
    weak: Weak<AppWindow>,
    cmd_tx: mpsc::Sender<Command>,
    playlist: SharedPlaylist,
    preview: SharedPreview,
    tracks: super::SharedTracks,
    project: super::SharedProject,
    zoom: SharedZoom,
    undo: SharedUndo,
) {
    let next = {
        let pl = playlist.lock().expect("playlist mutex poisoned");
        let mut u = undo.lock().expect("undo mutex poisoned");
        u.redo(&pl)
    };
    let Some(next) = next else { return };
    restore_playlist(weak, cmd_tx, playlist, tracks, project, preview, zoom, next);
}

fn on_save_project(
    weak: Weak<AppWindow>,
    playlist: SharedPlaylist,
    project_state: super::SharedProject,
    tracks_state: super::SharedTracks,
    zoom: SharedZoom,
) {
    let path = rfd::FileDialog::new()
        .add_filter("Cutoff project", &["json"])
        .set_title("Save project")
        .set_file_name("project.cutoff.json")
        .save_file();
    let Some(path) = path else { return };

    let project = {
        // Start from the live project (preserves user-added tracks)
        // and rebuild clip placement from the current playlist.
        let mut core_proj = project_state.lock().expect("project mutex poisoned").clone();
        {
            let pl = playlist.lock().expect("playlist mutex poisoned");
            core_proj.populate_from_playlist(&pl);
        }
        let tracks = tracks_state.lock().expect("tracks mutex poisoned");
        for (idx, t) in core_proj.tracks.iter_mut().enumerate() {
            t.muted = tracks.muted.get(idx).copied().unwrap_or(false);
            t.solo = tracks.soloed.get(idx).copied().unwrap_or(false);
            t.locked = tracks.locked.get(idx).copied().unwrap_or(false);
        }

        {
            let mut proj_state = project_state.lock().expect("project mutex poisoned");
            *proj_state = core_proj.clone();
        }

        let z = *zoom.lock().expect("zoom mutex poisoned");
        Project::new(core_proj, z)
    };
    if let Err(err) = project.save_to(&path) {
        tracing::warn!(error = %err, ?path, "save project failed");
        if let Some(window) = weak.upgrade() {
            window.set_status_text(SharedString::from(format!("Save failed: {err}")));
        }
        return;
    }
    if let Some(window) = weak.upgrade() {
        window.set_status_text(SharedString::from(format!(
            "Saved: {}",
            path.file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default()
        )));
    }
}

fn on_load_project(
    weak: Weak<AppWindow>,
    cmd_tx: mpsc::Sender<Command>,
    playlist: SharedPlaylist,
    project_state: super::SharedProject,
    tracks_state: super::SharedTracks,
    preview: SharedPreview,
    zoom: SharedZoom,
    undo: SharedUndo,
) {
    let path = rfd::FileDialog::new()
        .add_filter("Cutoff project", &["json"])
        .set_title("Open project")
        .pick_file();
    let Some(path) = path else { return };

    let project = match Project::load_from(&path) {
        Ok(p) => p,
        Err(err) => {
            tracing::warn!(error = %err, ?path, "load project failed");
            if let Some(window) = weak.upgrade() {
                window.set_status_text(SharedString::from(format!("Load failed: {err}")));
            }
            return;
        }
    };
    // Load is destructive of current state; reset undo so an immediate
    // Cmd+Z doesn't surprise the user with a step into the pre-load past.
    {
        let mut u = undo.lock().expect("undo mutex poisoned");
        u.clear();
    }
    {
        let mut z = zoom.lock().expect("zoom mutex poisoned");
        *z = if project.zoom_px_per_sec > 0.0 {
            project.zoom_px_per_sec.clamp(ZOOM_MIN, ZOOM_MAX)
        } else {
            ZOOM_DEFAULT
        };
    }

    let core_proj = project.core_project();
    {
        let mut proj = project_state.lock().expect("project mutex poisoned");
        *proj = core_proj.clone();
    }

    {
        let mut t = tracks_state.lock().expect("tracks mutex poisoned");
        t.muted = core_proj.tracks.iter().map(|tr| tr.muted).collect();
        t.soloed = core_proj.tracks.iter().map(|tr| tr.solo).collect();
        t.locked = core_proj.tracks.iter().map(|tr| tr.locked).collect();
    }

    let loaded_playlist = project.playlist_compat();
    restore_playlist(weak.clone(), cmd_tx, playlist, tracks_state, project_state, preview, zoom, loaded_playlist);
    if let Some(window) = weak.upgrade() {
        window.set_status_text(SharedString::from(format!(
            "Loaded: {}",
            path.file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default()
        )));
    }
}

fn on_toggle_master_mute(
    weak: Weak<AppWindow>,
    cmd_tx: mpsc::Sender<Command>,
    audio: super::SharedAudio,
) {
    let now_muted = {
        let mut a = audio.lock().expect("audio mutex poisoned");
        a.master_muted = !a.master_muted;
        a.master_muted
    };
    if let Err(err) = cmd_tx.try_send(Command::SetMasterMuted(now_muted)) {
        tracing::warn!(error = %err, "set-master-muted failed");
    }
    if let Some(window) = weak.upgrade() {
        window.set_master_muted(now_muted);
    }
}

fn on_move_to_track(
    weak: Weak<AppWindow>,
    cmd_tx: mpsc::Sender<Command>,
    playlist: SharedPlaylist,
    tracks: super::SharedTracks,
    project: super::SharedProject,
    zoom: SharedZoom,
    undo: SharedUndo,
    id: SharedString,
    target_track: u8,
) {
    let Ok(uuid) = Uuid::parse_str(id.as_str()) else { return };
    // Reject values that don't map to any current project track.
    {
        let proj = project.lock().expect("project mutex poisoned");
        let target_idx = proj.track_index_for_video_track_value(target_track);
        if target_idx >= proj.tracks.len() {
            return;
        }
    }
    {
        let mut pl = playlist.lock().expect("playlist mutex poisoned");
        if let Ok(mut u) = undo.lock() {
            u.checkpoint(&pl);
        }
        for c in pl.clips_mut() {
            if c.id == uuid {
                c.video_track = target_track;
                break;
            }
        }
    }
    if let Some(window) = weak.upgrade() {
        {
            let pl = playlist.lock().expect("playlist mutex poisoned");
            let z = *zoom.lock().expect("zoom mutex poisoned");
            models::sync_clips(&window, &pl);
            timeline_view::refresh_ruler(&window, &pl, z);
        }
        refresh_tracks_ui(&window, &playlist, &tracks, &project);
    }
    sync_preview(&playlist, &tracks, &project, &cmd_tx);
}

#[allow(clippy::too_many_arguments)]
fn on_detach_audio(
    weak: Weak<AppWindow>,
    cmd_tx: mpsc::Sender<Command>,
    playlist: SharedPlaylist,
    tracks: super::SharedTracks,
    project: super::SharedProject,
    zoom: SharedZoom,
    undo: SharedUndo,
    id: SharedString,
) {
    let Ok(uuid) = Uuid::parse_str(id.as_str()) else { return };
    {
        let mut pl = playlist.lock().expect("playlist mutex poisoned");
        checkpoint(&undo, &pl);
        if let Err(err) = services::detach_audio(&mut pl, uuid) {
            tracing::warn!(error = %err, "detach audio failed");
            return;
        }
    }
    if let Some(window) = weak.upgrade() {
        {
            let pl = playlist.lock().expect("playlist mutex poisoned");
            let z = *zoom.lock().expect("zoom mutex poisoned");
            models::sync_clips(&window, &pl);
            timeline_view::refresh_ruler(&window, &pl, z);
        }
        refresh_tracks_ui(&window, &playlist, &tracks, &project);
    }
    sync_preview(&playlist, &tracks, &project, &cmd_tx);
}

fn on_toggle_track_flag(
    weak: Weak<AppWindow>,
    cmd_tx: mpsc::Sender<Command>,
    playlist: SharedPlaylist,
    tracks: super::SharedTracks,
    project: super::SharedProject,
    idx: i32,
    flag: TrackFlag,
) {
    if idx < 0 {
        return;
    }
    let i = idx as usize;
    {
        let project_len = project.lock().expect("project mutex poisoned").tracks.len();
        let mut t = tracks.lock().expect("tracks mutex poisoned");
        t.resize(project_len);
        if i >= project_len {
            return;
        }
        match flag {
            TrackFlag::Muted => t.muted[i] = !t.muted[i],
            TrackFlag::Soloed => t.soloed[i] = !t.soloed[i],
            TrackFlag::Locked => t.locked[i] = !t.locked[i],
        }
    }
    if let Some(window) = weak.upgrade() {
        refresh_tracks_ui(&window, &playlist, &tracks, &project);
    }
    sync_preview(&playlist, &tracks, &project, &cmd_tx);
}

#[derive(Clone, Copy)]
enum TrackFlag { Muted, Soloed, Locked }

fn on_toggle_track_mute(
    weak: Weak<AppWindow>,
    cmd_tx: mpsc::Sender<Command>,
    playlist: SharedPlaylist,
    tracks: super::SharedTracks,
    project: super::SharedProject,
    idx: i32,
) {
    on_toggle_track_flag(weak, cmd_tx, playlist, tracks, project, idx, TrackFlag::Muted);
}

fn on_toggle_track_solo(
    weak: Weak<AppWindow>,
    cmd_tx: mpsc::Sender<Command>,
    playlist: SharedPlaylist,
    tracks: super::SharedTracks,
    project: super::SharedProject,
    idx: i32,
) {
    on_toggle_track_flag(weak, cmd_tx, playlist, tracks, project, idx, TrackFlag::Soloed);
}

fn on_toggle_track_lock(
    weak: Weak<AppWindow>,
    cmd_tx: mpsc::Sender<Command>,
    playlist: SharedPlaylist,
    tracks: super::SharedTracks,
    project: super::SharedProject,
    idx: i32,
) {
    on_toggle_track_flag(weak, cmd_tx, playlist, tracks, project, idx, TrackFlag::Locked);
}

/// Append a new track of the requested kind to the project, then refresh.
fn on_add_track(
    weak: Weak<AppWindow>,
    cmd_tx: mpsc::Sender<Command>,
    playlist: SharedPlaylist,
    tracks: super::SharedTracks,
    project: super::SharedProject,
    kind: video_merger_core::domain::TrackKind,
) {
    {
        let mut proj = project.lock().expect("project mutex poisoned");
        let name = proj.next_track_name(kind);
        proj.add_track(&name, kind);
        let new_len = proj.tracks.len();
        drop(proj);
        let mut ts = tracks.lock().expect("tracks mutex poisoned");
        ts.resize(new_len);
    }
    if let Some(window) = weak.upgrade() {
        refresh_tracks_ui(&window, &playlist, &tracks, &project);
    }
    sync_preview(&playlist, &tracks, &project, &cmd_tx);
}

/// Delete the track at `idx`. Clips on it are dropped; clips on tracks at
/// higher indices have their `video_track` value compacted.
fn on_delete_track(
    weak: Weak<AppWindow>,
    cmd_tx: mpsc::Sender<Command>,
    playlist: SharedPlaylist,
    tracks: super::SharedTracks,
    project: super::SharedProject,
    undo: SharedUndo,
    zoom: SharedZoom,
    idx: i32,
) {
    if idx < 0 {
        return;
    }
    let i = idx as usize;
    let removed_video_track_value: Option<u8> = {
        let mut proj = project.lock().expect("project mutex poisoned");
        if i >= proj.tracks.len() || proj.tracks.len() == 1 {
            return;
        }
        let v = proj.video_track_value_for_index(i);
        if let Err(err) = proj.remove_track(i) {
            tracing::warn!(error = %err, "remove_track failed");
            return;
        }
        Some(v)
    };
    if let Some(v) = removed_video_track_value {
        let mut pl = playlist.lock().expect("playlist mutex poisoned");
        checkpoint(&undo, &pl);
        let ids_to_drop: Vec<uuid::Uuid> = pl
            .clips()
            .iter()
            .filter(|c| c.video_track == v)
            .map(|c| c.id)
            .collect();
        for id in ids_to_drop {
            let _ = pl.remove(id);
        }
        // Compact the video_track values for clips whose values were greater
        // than `v` to mirror Project::remove_track's compaction.
        for c in pl.clips_mut() {
            if c.video_track > v {
                c.video_track -= 1;
            }
        }
    }
    {
        let mut ts = tracks.lock().expect("tracks mutex poisoned");
        ts.remove(i);
    }
    if let Some(window) = weak.upgrade() {
        {
            let pl = playlist.lock().expect("playlist mutex poisoned");
            let z = *zoom.lock().expect("zoom mutex poisoned");
            models::sync_clips(&window, &pl);
            timeline_view::refresh_ruler(&window, &pl, z);
        }
        refresh_tracks_ui(&window, &playlist, &tracks, &project);
    }
    sync_preview(&playlist, &tracks, &project, &cmd_tx);
}

fn on_reveal_in_finder(playlist: SharedPlaylist, id: SharedString) {
    let Ok(uuid) = Uuid::parse_str(id.as_str()) else { return };
    let path = {
        let pl = playlist.lock().expect("playlist mutex poisoned");
        pl.clips()
            .iter()
            .find(|c| c.id == uuid)
            .map(|c| c.path.clone())
    };
    let Some(path) = path else { return };
    if let Err(err) = opener::reveal(&path) {
        tracing::warn!(error = %err, ?path, "reveal in finder failed");
    }
}

fn on_export(
    weak: Weak<AppWindow>,
    cmd_tx: mpsc::Sender<Command>,
    playlist: SharedPlaylist,
    _project_state: super::SharedProject,
    tracks_state: super::SharedTracks,
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

    let (project, spec, render_plan) = {
        let mut proj = {
            let pl = playlist.lock().expect("playlist mutex poisoned");
            if pl.is_empty() {
                if let Some(window) = weak.upgrade() {
                    window.set_status_text("Add some clips first".into());
                }
                return;
            }
            // Start from the live project (keeps user-added tracks) and
            // refresh clip placement from the current playlist.
            let mut p = _project_state.lock().expect("project mutex poisoned").clone();
            p.populate_from_playlist(&pl);
            p
        };

        let tracks = tracks_state.lock().expect("tracks mutex poisoned");
        for (idx, t) in proj.tracks.iter_mut().enumerate() {
            t.muted = tracks.muted.get(idx).copied().unwrap_or(false);
            t.solo = tracks.soloed.get(idx).copied().unwrap_or(false);
            t.locked = tracks.locked.get(idx).copied().unwrap_or(false);
        }

        // Solo logic: if any track is soloed, all non-soloed tracks are muted.
        let any_solo = proj.tracks.iter().any(|t| t.solo);
        if any_solo {
            for t in &mut proj.tracks {
                if !t.solo {
                    t.muted = true;
                }
            }
        }

        let spec = ExportSpec {
            output_path: output_path.clone(),
            format: ContainerFormat::Mp4,
            quality: Quality::Lossless,
        };

        let render_plan = video_merger_engine::composite::build_render_plan(&proj);
        (proj, spec, render_plan)
    };

    let id = Uuid::new_v4();
    job_meta.lock().expect("job_meta mutex poisoned").insert(
        id,
        JobMeta {
            output_path: output_path.clone(),
            input_count: render_plan.inputs.len(),
            duration: Duration::from_micros(render_plan.total_duration_us.max(0) as u64),
        },
    );
    *active_job.lock().expect("active_job mutex poisoned") = Some(id);

    if let Some(window) = weak.upgrade() {
        window.set_exporting(true);
        window.set_export_progress(0.0);
        window.set_status_text("Exporting…".into());
    }

    if let Err(err) = cmd_tx.try_send(Command::Merge { id, project, spec }) {
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

fn on_selected_clip_volume_changed(
    weak: Weak<AppWindow>,
    cmd_tx: mpsc::Sender<Command>,
    playlist: SharedPlaylist,
    tracks: super::SharedTracks,
    project: super::SharedProject,
    zoom: SharedZoom,
    undo: SharedUndo,
    volume: f32,
) {
    if let Some(window) = weak.upgrade() {
        let sel_id = window.get_selected_id();
        let Ok(uuid) = Uuid::parse_str(sel_id.as_str()) else { return; };
        {
            let mut pl = playlist.lock().expect("playlist mutex poisoned");
            if let Ok(mut u) = undo.lock() {
                u.checkpoint(&pl);
            }
            if let Some(c) = pl.clips_mut().iter_mut().find(|clip| clip.id == uuid) {
                c.volume = volume;
            }
        }
        {
            let pl = playlist.lock().expect("playlist mutex poisoned");
            models::sync_clips(&window, &pl);
            let z = *zoom.lock().expect("zoom mutex poisoned");
            timeline_view::refresh_ruler(&window, &pl, z);
        }
        refresh_tracks_ui(&window, &playlist, &tracks, &project);
        sync_preview(&playlist, &tracks, &project, &cmd_tx);
    }
}

fn on_selected_clip_opacity_changed(
    weak: Weak<AppWindow>,
    cmd_tx: mpsc::Sender<Command>,
    playlist: SharedPlaylist,
    tracks: super::SharedTracks,
    project: super::SharedProject,
    _undo: SharedUndo,
    opacity: f32,
) {
    if let Some(_window) = weak.upgrade() {
        let sel_id = _window.get_selected_id();
        let Ok(uuid) = Uuid::parse_str(sel_id.as_str()) else { return; };
        {
            let mut pl = playlist.lock().expect("playlist mutex poisoned");
            if let Some(c) = pl.clips_mut().iter_mut().find(|clip| clip.id == uuid) {
                c.opacity = opacity;
            }
        }
        sync_preview(&playlist, &tracks, &project, &cmd_tx);
    }
}

fn on_selected_clip_scale_changed(
    weak: Weak<AppWindow>,
    cmd_tx: mpsc::Sender<Command>,
    playlist: SharedPlaylist,
    tracks: super::SharedTracks,
    project: super::SharedProject,
    _undo: SharedUndo,
    scale: f32,
) {
    if let Some(window) = weak.upgrade() {
        let sel_id = window.get_selected_id();
        let Ok(uuid) = Uuid::parse_str(sel_id.as_str()) else { return; };
        {
            let mut pl = playlist.lock().expect("playlist mutex poisoned");
            if let Some(c) = pl.clips_mut().iter_mut().find(|clip| clip.id == uuid) {
                c.scale = scale;
            }
        }
        window.set_selected_scale(scale);
        sync_preview(&playlist, &tracks, &project, &cmd_tx);
    }
}

fn on_selected_clip_position_x_changed(
    weak: Weak<AppWindow>,
    cmd_tx: mpsc::Sender<Command>,
    playlist: SharedPlaylist,
    tracks: super::SharedTracks,
    project: super::SharedProject,
    _undo: SharedUndo,
    pos_x: i32,
) {
    if let Some(window) = weak.upgrade() {
        let sel_id = window.get_selected_id();
        let Ok(uuid) = Uuid::parse_str(sel_id.as_str()) else { return; };
        {
            let mut pl = playlist.lock().expect("playlist mutex poisoned");
            if let Some(c) = pl.clips_mut().iter_mut().find(|clip| clip.id == uuid) {
                c.position_x = pos_x;
            }
        }
        window.set_selected_position_x(pos_x);
        sync_preview(&playlist, &tracks, &project, &cmd_tx);
    }
}

fn on_selected_clip_position_y_changed(
    weak: Weak<AppWindow>,
    cmd_tx: mpsc::Sender<Command>,
    playlist: SharedPlaylist,
    tracks: super::SharedTracks,
    project: super::SharedProject,
    _undo: SharedUndo,
    pos_y: i32,
) {
    if let Some(window) = weak.upgrade() {
        let sel_id = window.get_selected_id();
        let Ok(uuid) = Uuid::parse_str(sel_id.as_str()) else { return; };
        {
            let mut pl = playlist.lock().expect("playlist mutex poisoned");
            if let Some(c) = pl.clips_mut().iter_mut().find(|clip| clip.id == uuid) {
                c.position_y = pos_y;
            }
        }
        window.set_selected_position_y(pos_y);
        sync_preview(&playlist, &tracks, &project, &cmd_tx);
    }
}

/// Apply an `f32` transform edit (scale-x/y, rotation, …) to the selected clip.
/// Undo is checkpointed separately on `transform-gesture-begin`, so this only
/// mutates and re-syncs the preview.
fn on_selected_clip_transform_f32(
    weak: Weak<AppWindow>,
    cmd_tx: mpsc::Sender<Command>,
    playlist: SharedPlaylist,
    tracks: super::SharedTracks,
    project: super::SharedProject,
    value: f32,
    apply: impl Fn(&mut Clip, f32),
) {
    if let Some(window) = weak.upgrade() {
        let sel_id = window.get_selected_id();
        let Ok(uuid) = Uuid::parse_str(sel_id.as_str()) else { return; };
        {
            let mut pl = playlist.lock().expect("playlist mutex poisoned");
            if let Some(c) = pl.clips_mut().iter_mut().find(|clip| clip.id == uuid) {
                apply(c, value);
            }
        }
        sync_preview(&playlist, &tracks, &project, &cmd_tx);
    }
}

fn on_selected_clip_color_grading_changed(
    weak: Weak<AppWindow>,
    cmd_tx: mpsc::Sender<Command>,
    playlist: SharedPlaylist,
    tracks: super::SharedTracks,
    project: super::SharedProject,
    name: String,
    value: f32,
) {
    if let Some(window) = weak.upgrade() {
        let sel_id = window.get_selected_id();
        let Ok(uuid) = Uuid::parse_str(sel_id.as_str()) else { return; };
        {
            let mut pl = playlist.lock().expect("playlist mutex poisoned");
            if let Some(c) = pl.clips_mut().iter_mut().find(|clip| clip.id == uuid) {
                match name.as_str() {
                    "temperature" => c.color_grading.temperature = value,
                    "tint" => c.color_grading.tint = value,
                    "lift_r" => c.color_grading.lift_r = value,
                    "lift_g" => c.color_grading.lift_g = value,
                    "lift_b" => c.color_grading.lift_b = value,
                    "gamma_r" => c.color_grading.gamma_r = value,
                    "gamma_g" => c.color_grading.gamma_g = value,
                    "gamma_b" => c.color_grading.gamma_b = value,
                    "gain_r" => c.color_grading.gain_r = value,
                    "gain_g" => c.color_grading.gain_g = value,
                    "gain_b" => c.color_grading.gain_b = value,
                    "vignette" => c.color_grading.vignette = value,
                    _ => {}
                }
            }
        }
        sync_preview(&playlist, &tracks, &project, &cmd_tx);
    }
}

/// Boolean variant (flip-h / flip-v).
fn on_selected_clip_transform_bool(
    weak: Weak<AppWindow>,
    cmd_tx: mpsc::Sender<Command>,
    playlist: SharedPlaylist,
    tracks: super::SharedTracks,
    project: super::SharedProject,
    value: bool,
    apply: impl Fn(&mut Clip, bool),
) {
    if let Some(window) = weak.upgrade() {
        let sel_id = window.get_selected_id();
        let Ok(uuid) = Uuid::parse_str(sel_id.as_str()) else { return; };
        {
            let mut pl = playlist.lock().expect("playlist mutex poisoned");
            if let Some(c) = pl.clips_mut().iter_mut().find(|clip| clip.id == uuid) {
                apply(c, value);
            }
        }
        sync_preview(&playlist, &tracks, &project, &cmd_tx);
    }
}

/// The clip's *static* field value for a channel — captured as the value of a
/// freshly-added keyframe (it's what the NumberField shows).
fn channel_static_value(clip: &Clip, ch: TransformChannel) -> f32 {
    match ch {
        TransformChannel::Opacity => clip.opacity,
        TransformChannel::Scale => clip.scale,
        TransformChannel::ScaleX => clip.scale_x,
        TransformChannel::ScaleY => clip.scale_y,
        TransformChannel::PositionX => clip.position_x as f32,
        TransformChannel::PositionY => clip.position_y as f32,
        TransformChannel::Rotation => clip.rotation_deg,
    }
}

/// Clip-relative time (since the clip's visible start) at the current playhead,
/// clamped to the clip's effective duration. Uses the project placement
/// (`start_us`) the compositor renders with, falling back to the flat-playlist
/// offset if the project isn't populated yet.
fn clip_rel_us(
    playlist: &SharedPlaylist,
    project: &super::SharedProject,
    preview: &SharedPreview,
    uuid: Uuid,
) -> i64 {
    let playhead = preview.lock().map(|p| p.playhead_us).unwrap_or(0);
    let start_from_project = project.lock().ok().and_then(|p| {
        p.find_clip(uuid).map(|(ti, ci)| p.tracks[ti].clips[ci].start_us)
    });
    let (start, dur) = {
        let pl = playlist.lock().expect("playlist mutex poisoned");
        let dur = pl
            .clips()
            .iter()
            .find(|c| c.id == uuid)
            .map(|c| c.effective_duration_us())
            .unwrap_or(0);
        let start = start_from_project.unwrap_or_else(|| {
            timeline_view::selected_clip_window(&pl, Some(uuid)).map(|(o, _)| o).unwrap_or(0)
        });
        (start, dur)
    };
    (playhead - start).clamp(0, dur.max(0))
}

/// Push per-channel keyframe state (animated + key-at-playhead) to the window
/// so the diamond toggles render correctly. Call after selection, seek, or a
/// keyframe edit.
fn refresh_keyframe_flags(
    window: &AppWindow,
    playlist: &SharedPlaylist,
    project: &super::SharedProject,
    preview: &SharedPreview,
) {
    let Ok(uuid) = Uuid::parse_str(window.get_selected_id().as_str()) else { return; };
    let rel_us = clip_rel_us(playlist, project, preview, uuid);
    let pl = playlist.lock().expect("playlist mutex poisoned");
    let Some(c) = pl.clips().iter().find(|c| c.id == uuid) else { return; };
    let k = &c.transform_keys;

    window.set_selected_opacity_animated(k.opacity.is_animated());
    window.set_selected_opacity_key(k.opacity.has_key_near(rel_us, KEY_TOL_US));
    window.set_selected_scale_animated(k.scale.is_animated());
    window.set_selected_scale_key(k.scale.has_key_near(rel_us, KEY_TOL_US));
    window.set_selected_position_x_animated(k.position_x.is_animated());
    window.set_selected_position_x_key(k.position_x.has_key_near(rel_us, KEY_TOL_US));
    window.set_selected_position_y_animated(k.position_y.is_animated());
    window.set_selected_position_y_key(k.position_y.has_key_near(rel_us, KEY_TOL_US));
    window.set_selected_rotation_animated(k.rotation_deg.is_animated());
    window.set_selected_rotation_key(k.rotation_deg.has_key_near(rel_us, KEY_TOL_US));

    let opacity_interp = k.opacity.keyframe_interp_near(rel_us, KEY_TOL_US)
        .map(|i| match i {
            video_merger_core::domain::Interp::Hold => "hold",
            video_merger_core::domain::Interp::Bezier => "bezier",
            video_merger_core::domain::Interp::Linear => "linear",
        })
        .unwrap_or("linear");
    window.set_selected_opacity_interp(slint::SharedString::from(opacity_interp));

    let rotation_interp = k.rotation_deg.keyframe_interp_near(rel_us, KEY_TOL_US)
        .map(|i| match i {
            video_merger_core::domain::Interp::Hold => "hold",
            video_merger_core::domain::Interp::Bezier => "bezier",
            video_merger_core::domain::Interp::Linear => "linear",
        })
        .unwrap_or("linear");
    window.set_selected_rotation_interp(slint::SharedString::from(rotation_interp));
}

/// Toggle a keyframe at the current playhead for `channel_id`: remove one if it
/// sits within tolerance, otherwise add one capturing the current value.
#[allow(clippy::too_many_arguments)]
fn on_selected_clip_keyframe_toggle(
    weak: Weak<AppWindow>,
    cmd_tx: mpsc::Sender<Command>,
    playlist: SharedPlaylist,
    tracks: super::SharedTracks,
    project: super::SharedProject,
    preview: SharedPreview,
    undo: SharedUndo,
    channel_id: i32,
) {
    let Some(window) = weak.upgrade() else { return; };
    let Ok(uuid) = Uuid::parse_str(window.get_selected_id().as_str()) else { return; };
    let Some(ch) = TransformChannel::from_id(channel_id) else { return; };
    let rel_us = clip_rel_us(&playlist, &project, &preview, uuid);
    {
        let mut pl = playlist.lock().expect("playlist mutex poisoned");
        checkpoint(&undo, &pl);
        if let Some(c) = pl.clips_mut().iter_mut().find(|c| c.id == uuid) {
            let value = channel_static_value(c, ch);
            let track = c.transform_keys.channel_mut(ch);
            if track.has_key_near(rel_us, KEY_TOL_US) {
                track.remove_near(rel_us, KEY_TOL_US);
            } else {
                track.upsert(rel_us, value, KEY_TOL_US);
            }
        }
    }
    sync_preview(&playlist, &tracks, &project, &cmd_tx);
    refresh_keyframe_flags(&window, &playlist, &project, &preview);
}

fn on_selected_clip_keyframe_interp_changed(
    weak: Weak<AppWindow>,
    cmd_tx: mpsc::Sender<Command>,
    playlist: SharedPlaylist,
    tracks: super::SharedTracks,
    project: super::SharedProject,
    preview: SharedPreview,
    undo: SharedUndo,
    channel_id: i32,
    mode: String,
) {
    let Some(window) = weak.upgrade() else { return; };
    let Ok(uuid) = Uuid::parse_str(window.get_selected_id().as_str()) else { return; };
    let Some(ch) = TransformChannel::from_id(channel_id) else { return; };
    let rel_us = clip_rel_us(&playlist, &project, &preview, uuid);
    let interp = match mode.as_str() {
        "hold" => video_merger_core::domain::Interp::Hold,
        "bezier" => video_merger_core::domain::Interp::Bezier,
        _ => video_merger_core::domain::Interp::Linear,
    };
    {
        let mut pl = playlist.lock().expect("playlist mutex poisoned");
        checkpoint(&undo, &pl);
        if let Some(c) = pl.clips_mut().iter_mut().find(|c| c.id == uuid) {
            c.transform_keys.channel_mut(ch).set_interp_near(rel_us, interp, KEY_TOL_US);
        }
    }
    sync_preview(&playlist, &tracks, &project, &cmd_tx);
    refresh_keyframe_flags(&window, &playlist, &project, &preview);
}

fn on_selected_clip_muted_changed(
    weak: Weak<AppWindow>,
    cmd_tx: mpsc::Sender<Command>,
    playlist: SharedPlaylist,
    tracks: super::SharedTracks,
    project: super::SharedProject,
    zoom: SharedZoom,
    undo: SharedUndo,
    muted: bool,
) {
    if let Some(window) = weak.upgrade() {
        let sel_id = window.get_selected_id();
        let Ok(uuid) = Uuid::parse_str(sel_id.as_str()) else { return; };
        {
            let mut pl = playlist.lock().expect("playlist mutex poisoned");
            if let Ok(mut u) = undo.lock() {
                u.checkpoint(&pl);
            }
            if let Some(c) = pl.clips_mut().iter_mut().find(|clip| clip.id == uuid) {
                c.muted = muted;
            }
        }
        {
            let pl = playlist.lock().expect("playlist mutex poisoned");
            models::sync_clips(&window, &pl);
            let z = *zoom.lock().expect("zoom mutex poisoned");
            timeline_view::refresh_ruler(&window, &pl, z);
        }
        refresh_tracks_ui(&window, &playlist, &tracks, &project);
        sync_preview(&playlist, &tracks, &project, &cmd_tx);
    }
}

