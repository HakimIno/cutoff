//! Adapters between domain types and the Slint-generated UI structs.

use std::path::Path;
use std::rc::Rc;

use slint::{Image, Model, ModelRc, SharedString, VecModel};
use video_merger_core::domain::{Clip, Playlist, Project, TrackKind};
use video_merger_engine::audio::waveform as wf;
use video_merger_ui::{AppWindow, ClipData, TrackData};

use crate::view_model::timeline_vm::ClipRow;
use super::TracksState;

/// Down-sample waveform peaks into the ~200 bars the UI strip can render
/// without overdraw. Returns abs peak per output bucket as a value in 0..1.
fn waveform_to_bars(path: &Path, bars: usize) -> Vec<f32> {
    let Ok(peaks) = wf::load_waveform(path) else {
        return Vec::new();
    };
    if peaks.peaks.is_empty() {
        return Vec::new();
    }
    let n_in = peaks.peaks.len();
    let bars = bars.min(n_in).max(1);
    let mut out = Vec::with_capacity(bars);
    for i in 0..bars {
        let start = i * n_in / bars;
        let end = ((i + 1) * n_in / bars).max(start + 1);
        let mut peak = 0.0f32;
        for j in start..end {
            let (mn, mx) = peaks.peaks[j];
            let a = mn.abs().max(mx.abs());
            if a > peak {
                peak = a;
            }
        }
        out.push(peak.clamp(0.0, 1.0));
    }
    out
}

/// Convert a domain `Clip` into the Slint-side `ClipData` struct.
pub fn clip_to_data(clip: &Clip) -> ClipData {
    let row = ClipRow::from(clip);
    let thumbs: Vec<Image> = clip
        .thumbnails
        .iter()
        .filter_map(|p| load_image(p))
        .collect();
    let peaks: Vec<f32> = clip
        .waveform_path
        .as_deref()
        .map(|p| waveform_to_bars(p, 200))
        .unwrap_or_default();
    ClipData {
        id: SharedString::from(row.id),
        name: SharedString::from(row.name),
        duration: SharedString::from(row.duration),
        resolution: SharedString::from(row.resolution),
        duration_secs: row.duration_secs,
        thumbnails: ModelRc::from(Rc::new(VecModel::from(thumbs))),
        waveform_peaks: ModelRc::from(Rc::new(VecModel::from(peaks))),
        muted: clip.muted,
        video_track: clip.video_track as i32,
    }
}

fn load_image(path: &Path) -> Option<Image> {
    Image::load_from_path(path).ok()
}

/// Append a new clip to the flat sidebar model. The per-track display is
/// refreshed separately by `sync_tracks` (called from `sync_clips`).
pub fn push_clip(window: &AppWindow, data: ClipData) {
    let model = window.get_clips();
    if let Some(vm) = model.as_any().downcast_ref::<VecModel<ClipData>>() {
        vm.push(data);
        window.set_clip_count(vm.row_count() as i32);
    }
}

/// Rebuild **both** the flat sidebar model and the per-track timeline model
/// from the playlist. Call after any structural mutation (add / remove /
/// reorder / track-list change).
pub fn sync_clips(window: &AppWindow, playlist: &Playlist) {
    let model = window.get_clips();
    if let Some(vm) = model.as_any().downcast_ref::<VecModel<ClipData>>() {
        while vm.row_count() > 0 {
            vm.remove(0);
        }
        for clip in playlist.clips() {
            vm.push(clip_to_data(clip));
        }
        window.set_clip_count(vm.row_count() as i32);
    }
}

/// Rebuild the dynamic per-track UI model from `project` + `tracks_state`.
/// Each `TrackData` carries its own embedded `[ClipData]` model.
pub fn sync_tracks(window: &AppWindow, project: &Project, tracks_state: &TracksState) {
    let tracks_model = window.get_tracks();
    let Some(vm) = tracks_model.as_any().downcast_ref::<VecModel<TrackData>>() else {
        return;
    };
    while vm.row_count() > 0 {
        vm.remove(0);
    }
    for (idx, track) in project.tracks.iter().enumerate() {
        let clip_rows: Vec<ClipData> = track
            .clips
            .iter()
            .map(|tc| clip_to_data(&tc.clip))
            .collect();
        vm.push(TrackData {
            name: SharedString::from(track.name.clone()),
            is_audio: matches!(track.kind, TrackKind::Audio),
            clips: ModelRc::from(Rc::new(VecModel::from(clip_rows))),
            muted: tracks_state.muted.get(idx).copied().unwrap_or(false),
            soloed: tracks_state.soloed.get(idx).copied().unwrap_or(false),
            locked: tracks_state.locked.get(idx).copied().unwrap_or(false),
            video_track_value: project.video_track_value_for_index(idx) as i32,
        });
    }
}

/// Replace one clip row's thumbnails by clip id (no full sync). Updates
/// both the flat sidebar model and the corresponding per-track lane.
pub fn refresh_clip_thumbnails(window: &AppWindow, playlist: &Playlist, clip_id: uuid::Uuid) {
    let Some(clip) = playlist.clips().iter().find(|c| c.id == clip_id) else {
        return;
    };
    let id_str = clip.id.to_string();
    let new_data = clip_to_data(clip);
    let update_clip_model = |model: slint::ModelRc<ClipData>| {
        if let Some(vm) = model.as_any().downcast_ref::<VecModel<ClipData>>() {
            for i in 0..vm.row_count() {
                if let Some(row) = vm.row_data(i) {
                    if row.id.as_str() == id_str {
                        vm.set_row_data(i, new_data.clone());
                        break;
                    }
                }
            }
        }
    };

    // Flat sidebar model.
    update_clip_model(window.get_clips());

    // Per-track models embedded in the `tracks` VecModel.
    let tracks = window.get_tracks();
    if let Some(tvm) = tracks.as_any().downcast_ref::<VecModel<TrackData>>() {
        for i in 0..tvm.row_count() {
            if let Some(td) = tvm.row_data(i) {
                update_clip_model(td.clips);
            }
        }
    }
}
