//! Adapters between domain types and the Slint-generated UI structs.

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::OnceLock;

use slint::{Image, Model, ModelRc, SharedString, VecModel};
use video_merger_core::domain::{Clip, Playlist, Project, TrackClip, TrackKind};
use video_merger_engine::audio::waveform as wf;
use video_merger_ui::{AppWindow, ClipData, TrackData};

use crate::view_model::timeline_vm::ClipRow;
use super::TracksState;

static DATA_DIR: OnceLock<PathBuf> = OnceLock::new();

pub fn set_data_dir(path: PathBuf) {
    let _ = DATA_DIR.set(path);
}

struct WaveformBars {
    peaks: Vec<f32>,
    mins: Vec<f32>,
    maxs: Vec<f32>,
}

/// Down-sample waveform buckets for the UI without overdraw. `mins`/`maxs`
/// preserve the actual waveform shape; `peaks` is kept for fallback rendering.
fn waveform_to_bars(path: &Path, bars: usize) -> WaveformBars {
    let Ok(peaks) = wf::load_waveform(path) else {
        return WaveformBars::default();
    };
    if peaks.peaks.is_empty() {
        return WaveformBars::default();
    }
    let n_in = peaks.peaks.len();
    let bars = bars.min(n_in).max(1);
    let mut out_peaks = Vec::with_capacity(bars);
    let mut out_mins = Vec::with_capacity(bars);
    let mut out_maxs = Vec::with_capacity(bars);
    for i in 0..bars {
        let start = i * n_in / bars;
        let end = ((i + 1) * n_in / bars).max(start + 1);
        let mut peak = 0.0f32;
        let mut min_v = 0.0f32;
        let mut max_v = 0.0f32;
        for j in start..end {
            let (mn, mx) = peaks.peaks[j];
            if mn < min_v {
                min_v = mn;
            }
            if mx > max_v {
                max_v = mx;
            }
            let a = mn.abs().max(mx.abs());
            if a > peak {
                peak = a;
            }
        }
        out_peaks.push(peak.clamp(0.0, 1.0));
        out_mins.push(min_v.clamp(-1.0, 1.0));
        out_maxs.push(max_v.clamp(-1.0, 1.0));
    }
    let max_peak = out_peaks.iter().copied().fold(0.0f32, f32::max);
    if max_peak > 0.0001 {
        // Timeline waveforms are visual meters, not calibrated audio meters.
        // Normalize per clip so quiet camera audio doesn't collapse into a flat line.
        let gain = (0.82 / max_peak).clamp(1.0, 40.0);
        for peak in &mut out_peaks {
            *peak = (*peak * gain).clamp(0.0, 1.0);
        }
        for value in &mut out_mins {
            *value = (*value * gain).clamp(-1.0, 1.0);
        }
        for value in &mut out_maxs {
            *value = (*value * gain).clamp(-1.0, 1.0);
        }
    }
    WaveformBars {
        peaks: out_peaks,
        mins: out_mins,
        maxs: out_maxs,
    }
}

impl Default for WaveformBars {
    fn default() -> Self {
        Self {
            peaks: Vec::new(),
            mins: Vec::new(),
            maxs: Vec::new(),
        }
    }
}

/// Convert a domain `Clip` into the Slint-side `ClipData` struct.
pub fn clip_to_data(clip: &Clip) -> ClipData {
    let row = ClipRow::from(clip);
    let thumb_paths = asset_thumb_paths(clip);
    let thumbs: Vec<Image> = thumb_paths.iter().filter_map(|p| load_image(p)).collect();
    let waveform_path = asset_waveform_path(clip);
    let waveform = waveform_path
        .as_deref()
        .map(|p| waveform_to_bars(p, 200))
        .unwrap_or_default();
    ClipData {
        id: SharedString::from(row.id),
        name: SharedString::from(row.name),
        duration: SharedString::from(row.duration),
        resolution: SharedString::from(row.resolution),
        duration_secs: row.duration_secs,
        // Filled in per-track by `sync_tracks` (the flat sidebar list has no
        // timeline position, so it stays 0 there).
        start_secs: 0.0,
        thumbnails: ModelRc::from(Rc::new(VecModel::from(thumbs))),
        waveform_peaks: ModelRc::from(Rc::new(VecModel::from(waveform.peaks))),
        waveform_mins: ModelRc::from(Rc::new(VecModel::from(waveform.mins))),
        waveform_maxs: ModelRc::from(Rc::new(VecModel::from(waveform.maxs))),
        muted: clip.muted,
        video_track: clip.video_track as i32,
    }
}

fn load_image(path: &Path) -> Option<Image> {
    Image::load_from_path(path).ok()
}

fn asset_thumb_paths(clip: &Clip) -> Vec<PathBuf> {
    if !clip.thumbnails.is_empty() {
        return clip.thumbnails.clone();
    }
    let Some(data_dir) = DATA_DIR.get() else {
        return Vec::new();
    };
    let dir = data_dir.join("thumbs").join(clip.id.to_string());
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .and_then(|ext| ext.to_str())
                .map(|ext| ext.eq_ignore_ascii_case("jpg") || ext.eq_ignore_ascii_case("jpeg"))
                .unwrap_or(false)
        })
        .collect();
    paths.sort();
    paths
}

fn asset_waveform_path(clip: &Clip) -> Option<PathBuf> {
    if let Some(path) = &clip.waveform_path {
        return Some(path.clone());
    }
    let path = DATA_DIR
        .get()
        .map(|data_dir| data_dir.join("waveforms").join(format!("{}.bin", clip.id)))?;
    path.exists().then_some(path)
}

fn track_clip_rows(track_clips: &[TrackClip]) -> Vec<ClipData> {
    track_clips
        .iter()
        .map(|tc| {
            let mut d = clip_to_data(&tc.clip);
            // Position the card on the timeline at the clip's real start.
            d.start_secs = tc.start_us as f32 / 1_000_000.0;
            d
        })
        .collect()
}

fn paired_video_track_name(audio_track_name: &str) -> Option<String> {
    audio_track_name
        .strip_prefix('A')
        .filter(|suffix| !suffix.is_empty())
        .map(|suffix| format!("V{suffix}"))
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
        let mut display_only = false;
        let clip_rows = if matches!(track.kind, TrackKind::Audio) && track.clips.is_empty() {
            paired_video_track_name(&track.name)
                .and_then(|video_name| {
                    project
                        .tracks
                        .iter()
                        .find(|candidate| {
                            matches!(candidate.kind, TrackKind::Video)
                                && candidate.name == video_name
                                && !candidate.clips.is_empty()
                        })
                })
                .map(|video_track| {
                    display_only = true;
                    track_clip_rows(&video_track.clips)
                })
                .unwrap_or_else(|| track_clip_rows(&track.clips))
        } else {
            track_clip_rows(&track.clips)
        };
        vm.push(TrackData {
            name: SharedString::from(track.name.clone()),
            is_audio: matches!(track.kind, TrackKind::Audio),
            clips: ModelRc::from(Rc::new(VecModel::from(clip_rows))),
            display_only,
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
