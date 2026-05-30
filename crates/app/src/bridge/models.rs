//! Adapters between domain types and the Slint-generated UI structs.

use std::path::Path;
use std::rc::Rc;

use slint::{Image, Model, ModelRc, SharedString, VecModel};
use video_merger_core::domain::{Clip, Playlist};
use video_merger_ui::{AppWindow, ClipData};

use crate::view_model::timeline_vm::ClipRow;

/// Convert a domain `Clip` into the Slint-side `ClipData` struct.
pub fn clip_to_data(clip: &Clip) -> ClipData {
    let row = ClipRow::from(clip);
    let thumbs: Vec<Image> = clip
        .thumbnails
        .iter()
        .filter_map(|p| load_image(p))
        .collect();
    ClipData {
        id: SharedString::from(row.id),
        name: SharedString::from(row.name),
        duration: SharedString::from(row.duration),
        resolution: SharedString::from(row.resolution),
        duration_secs: row.duration_secs,
        thumbnails: ModelRc::from(Rc::new(VecModel::from(thumbs))),
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
}

/// Replace one row's thumbnails by clip id (no full sync).
pub fn refresh_clip_thumbnails(window: &AppWindow, playlist: &Playlist, clip_id: uuid::Uuid) {
    let model = window.get_clips();
    let Some(vm) = model.as_any().downcast_ref::<VecModel<ClipData>>() else {
        return;
    };
    let Some(clip) = playlist.clips().iter().find(|c| c.id == clip_id) else {
        return;
    };
    let id_str = clip.id.to_string();
    for i in 0..vm.row_count() {
        if let Some(row) = vm.row_data(i) {
            if row.id.as_str() == id_str {
                vm.set_row_data(i, clip_to_data(clip));
                break;
            }
        }
    }
}
