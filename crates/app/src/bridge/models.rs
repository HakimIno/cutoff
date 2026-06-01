//! Adapters between domain types and the Slint-generated UI structs.

use std::path::Path;
use std::rc::Rc;

use slint::{Image, Model, ModelRc, SharedString, VecModel};
use video_merger_core::domain::{Clip, Playlist};
use video_merger_engine::audio::waveform as wf;
use video_merger_ui::{AppWindow, ClipData};

use crate::view_model::timeline_vm::ClipRow;

/// Down-sample waveform peaks (one bucket per ~10ms) into the ~200 bars
/// the UI strip can render without overdraw. Returns abs peak per output
/// bucket as a value in 0..1.
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

/// Push a single row into the timeline model attached to `window`.
///
/// Must be called on the Slint event-loop thread.
pub fn push_clip(window: &AppWindow, data: ClipData) {
    let model = window.get_clips();
    if let Some(vm) = model.as_any().downcast_ref::<VecModel<ClipData>>() {
        vm.push(data.clone());
        window.set_clip_count(vm.row_count() as i32);
    }
    // Mirror into the corresponding per-track lane.
    let lane = match data.video_track {
        1 => window.get_clips_v2(),
        0 => window.get_clips_v1(),
        2 => window.get_clips_a1(),
        3 => window.get_clips_a2(),
        _ => window.get_clips_v1(),
    };
    if let Some(vm) = lane.as_any().downcast_ref::<VecModel<ClipData>>() {
        vm.push(data);
    }
}

/// Replace the entire contents of the VecModel from the current Playlist.
/// Used after remove / reorder when an incremental update is awkward.
pub fn sync_clips(window: &AppWindow, playlist: &Playlist) {
    let model = window.get_clips();
    let Some(vm) = model.as_any().downcast_ref::<VecModel<ClipData>>() else {
        return;
    };
    while vm.row_count() > 0 {
        vm.remove(0);
    }
    for clip in playlist.clips() {
        vm.push(clip_to_data(clip));
    }
    window.set_clip_count(vm.row_count() as i32);
    // Mirror into per-track models so the multi-lane Timeline can render
    // V2 above V1 without duplicating model bookkeeping in Slint.
    sync_per_track(window, playlist);
}

fn sync_per_track(window: &AppWindow, playlist: &Playlist) {
    let push_lane = |lane_track: u8, getter: fn(&AppWindow) -> slint::ModelRc<ClipData>| {
        let model = getter(window);
        let Some(vm) = model.as_any().downcast_ref::<VecModel<ClipData>>() else {
            return;
        };
        while vm.row_count() > 0 {
            vm.remove(0);
        }
        for clip in playlist.clips() {
            if clip.video_track == lane_track {
                vm.push(clip_to_data(clip));
            }
        }
    };
    push_lane(0, AppWindow::get_clips_v1);
    push_lane(1, AppWindow::get_clips_v2);
    push_lane(2, AppWindow::get_clips_a1);
    push_lane(3, AppWindow::get_clips_a2);
}

/// Replace one row's thumbnails by clip id (no full sync).
pub fn refresh_clip_thumbnails(window: &AppWindow, playlist: &Playlist, clip_id: uuid::Uuid) {
    let Some(clip) = playlist.clips().iter().find(|c| c.id == clip_id) else {
        return;
    };
    let id_str = clip.id.to_string();
    let new_data = clip_to_data(clip);
    let update = |model: slint::ModelRc<ClipData>| {
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
    update(window.get_clips());
    update(window.get_clips_v1());
    update(window.get_clips_v2());
    update(window.get_clips_a1());
    update(window.get_clips_a2());
}
