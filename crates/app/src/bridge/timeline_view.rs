//! Helpers that translate playlist + zoom state into Slint-side timeline
//! properties (ruler ticks, total width, global playhead fraction).
//!
//! Drag-and-drop math lives here too. The timeline renders four lanes
//! (V1, V2, A1, A2) but the playlist is a single `Vec<Clip>` where each
//! clip carries its `video_track`. A drag operates *inside* the source
//! clip's track only — horizontal reordering does not change track
//! assignment. To compute the drop indicator and final reorder we have
//! to project the playlist down to the source's track, do the math in
//! lane-local coordinates, then map the destination back to a global
//! playlist index for `Playlist::reorder`.

use std::rc::Rc;

use slint::{ModelRc, SharedString, VecModel};
use uuid::Uuid;
use video_merger_core::domain::Playlist;
use video_merger_ui::{AppWindow, RulerTick};

use crate::view_model::ruler_vm;

/// Recompute and push ruler ticks + total timeline width based on the current
/// playlist duration and zoom factor.
pub fn refresh_ruler(window: &AppWindow, playlist: &Playlist, px_per_sec: f32) {
    let total_secs = total_duration_secs(playlist);
    let total_width_px = (total_secs * px_per_sec).max(1.0);

    let ticks = ruler_vm::ticks(px_per_sec, total_secs, 80.0);
    let tick_rows: Vec<RulerTick> = ticks
        .iter()
        .map(|t| RulerTick {
            x: t.x_px as f32,
            label: SharedString::from(if t.major {
                ruler_vm::format_label_secs(t.label_secs)
            } else {
                String::new()
            }),
            major: t.major,
        })
        .collect();

    window.set_ruler_ticks(ModelRc::from(Rc::new(VecModel::from(tick_rows))));
    window.set_timeline_total_width(total_width_px);
    window.set_px_per_sec(px_per_sec);
}

/// Selected clip's `(offset_us, duration_us)` on the timeline, if present.
pub fn selected_clip_window(playlist: &Playlist, selected_id: Option<Uuid>) -> Option<(i64, i64)> {
    let id = selected_id?;
    let mut offset_us: i64 = 0;
    for clip in playlist.clips() {
        let dur_us = clip.effective_duration_us();
        if clip.id == id {
            return Some((offset_us, dur_us));
        }
        offset_us += dur_us;
    }
    None
}

pub fn total_duration_us(playlist: &Playlist) -> i64 {
    playlist
        .clips()
        .iter()
        .map(|c| c.effective_duration_us())
        .sum()
}

pub fn total_duration_secs(playlist: &Playlist) -> f32 {
    (total_duration_us(playlist) as f64 / 1_000_000.0) as f32
}

// Layout constants — keep in sync with `timeline.slint`. The TrackLane uses
// `padding-left: Theme.spacing-xs` (3px) and inter-card `spacing: 1px`.
pub const LANE_PADDING_PX: f32 = 3.0;
pub const CARD_SPACING_PX: f32 = 1.0;

pub const CARD_MIN_PX: f32 = 120.0;
pub const CARD_MAX_PX: f32 = 800.0;

pub fn card_width(duration_secs: f32, px_per_sec: f32) -> f32 {
    (duration_secs * px_per_sec).clamp(CARD_MIN_PX, CARD_MAX_PX)
}

/// Find a clip by id and return its `(global_idx, track_local_idx, track)`.
pub fn locate_clip(playlist: &Playlist, clip_id: Uuid) -> Option<(usize, usize, u8)> {
    let clips = playlist.clips();
    let global_idx = clips.iter().position(|c| c.id == clip_id)?;
    let track = clips[global_idx].video_track;
    let local_idx = clips
        .iter()
        .take(global_idx)
        .filter(|c| c.video_track == track)
        .count();
    Some((global_idx, local_idx, track))
}

/// Cumulative left-edge positions for each insertion gap **inside one
/// track**. `gaps[i]` is the x position where a card inserted at the
/// `i`-th lane-local slot would land. `gaps.len() == lane_count + 1`.
pub fn gap_positions_in_track(playlist: &Playlist, px_per_sec: f32, track: u8) -> Vec<f32> {
    let mut x = LANE_PADDING_PX;
    let mut gaps = Vec::new();
    gaps.push(x);
    for clip in playlist.clips().iter().filter(|c| c.video_track == track) {
        let w = card_width(clip.effective_duration().as_secs_f32(), px_per_sec);
        x += w + CARD_SPACING_PX;
        gaps.push(x);
    }
    gaps
}

#[derive(Debug, Clone, PartialEq)]
pub struct DropOutcome {
    /// Lane-local gap index (0..=lane_count).
    pub local_gap: usize,
    /// X pixel coord (in lane coordinates) where the drop indicator renders.
    pub indicator_x_px: f32,
    /// `Some((from_global, to_global))` to pass to `Playlist::reorder`, or
    /// `None` if the drop is a no-op (dropped on or just after the source's
    /// own slot).
    pub reorder: Option<(usize, usize)>,
}

/// Decide where the clip currently being dragged should drop.
///
/// All math happens inside the source clip's track lane. `delta_px` is the
/// horizontal pointer displacement from where the press started, in lane
/// pixels.
pub fn drop_target_for_clip(
    playlist: &Playlist,
    px_per_sec: f32,
    source_clip_id: Uuid,
    delta_px: f32,
) -> Option<DropOutcome> {
    let (from_global, from_local, track) = locate_clip(playlist, source_clip_id)?;

    let lane_clips: Vec<&_> = playlist
        .clips()
        .iter()
        .filter(|c| c.video_track == track)
        .collect();
    if lane_clips.is_empty() {
        return None;
    }

    let gaps = gap_positions_in_track(playlist, px_per_sec, track);
    let widths: Vec<f32> = lane_clips
        .iter()
        .map(|c| card_width(c.effective_duration().as_secs_f32(), px_per_sec))
        .collect();

    let orig_center = gaps[from_local] + widths[from_local] / 2.0;
    let new_center = orig_center + delta_px;

    // Snap to the nearest gap.
    let mut best_idx = 0usize;
    let mut best_dist = f32::INFINITY;
    for (i, gx) in gaps.iter().enumerate() {
        let d = (gx - new_center).abs();
        if d < best_dist {
            best_dist = d;
            best_idx = i;
        }
    }

    // Dropping at the source's own slot (gap == from_local or from_local+1)
    // is a no-op — we still want a sticky indicator there, but no reorder.
    if best_idx == from_local || best_idx == from_local + 1 {
        return Some(DropOutcome {
            local_gap: best_idx,
            indicator_x_px: gaps[best_idx],
            reorder: None,
        });
    }

    // Translate lane-local destination back to a global playlist index.
    //
    // `Playlist::reorder(from, to)` does `remove(from); insert(to, clip)`.
    // `to` is interpreted against the post-remove list and the existing
    // implementation rejects `to >= original_len`, so the maximum valid
    // value is `original_len - 1` (insert at the very end).
    //
    // The gap index `best_idx` is in the *original* lane layout (where the
    // source still appears). When dropping *after* the source's slot, the
    // post-remove lane has one fewer element and we must subtract 1.
    let pos_in_post_remove_lane = if best_idx > from_local {
        best_idx - 1
    } else {
        best_idx
    };

    let post_remove_track_indices: Vec<usize> = playlist
        .clips()
        .iter()
        .enumerate()
        .filter(|(i, c)| *i != from_global && c.video_track == track)
        .map(|(i, _)| if i > from_global { i - 1 } else { i })
        .collect();

    let to_global = if pos_in_post_remove_lane < post_remove_track_indices.len() {
        post_remove_track_indices[pos_in_post_remove_lane]
    } else {
        // Insert at the very end of the playlist.
        playlist.clips().len() - 1
    };

    Some(DropOutcome {
        local_gap: best_idx,
        indicator_x_px: gaps[best_idx],
        reorder: Some((from_global, to_global)),
    })
}

/// Map a fraction of the entire timeline (0..1) back to a clip + local pts.
#[allow(dead_code)]
pub fn fraction_to_clip_local(
    playlist: &Playlist,
    fraction: f32,
) -> Option<(Uuid, i64)> {
    let total_us = total_duration_us(playlist);
    if total_us <= 0 {
        return None;
    }
    let f = fraction.clamp(0.0, 1.0) as f64;
    let target_us = (total_us as f64 * f) as i64;
    let mut offset_us: i64 = 0;
    for clip in playlist.clips() {
        let dur_us = clip.effective_duration_us();
        if target_us < offset_us + dur_us {
            return Some((clip.id, (target_us - offset_us).max(0)));
        }
        offset_us += dur_us;
    }
    // Past the end → last clip's last frame.
    playlist
        .clips()
        .last()
        .map(|c| (c.id, c.effective_duration_us() - 1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use video_merger_core::domain::{Clip, CodecProfile, MediaInfo, Resolution};

    fn fake_clip(secs: f32, track: u8) -> Clip {
        let mut c = Clip::new(
            std::path::PathBuf::from(format!("/tmp/{}-{}.mp4", track, secs)),
            MediaInfo {
                duration: Duration::from_secs_f32(secs),
                profile: CodecProfile {
                    video_codec: "h264".into(),
                    audio_codec: "aac".into(),
                    resolution: Resolution { width: 1920, height: 1080 },
                    frame_rate_mhz: 30_000,
                    pixel_format: "yuv420p".into(),
                },
            },
        );
        c.video_track = track;
        c
    }

    /// Build a playlist from a list of `(track, duration_secs)` pairs and
    /// return both the playlist and the resulting clip ids in order.
    fn build(rows: &[(u8, f32)]) -> (Playlist, Vec<Uuid>) {
        let mut pl = Playlist::new();
        let mut ids = Vec::new();
        for &(t, d) in rows {
            let c = fake_clip(d, t);
            ids.push(c.id);
            pl.push(c);
        }
        (pl, ids)
    }

    // --- locate_clip ---------------------------------------------------------

    #[test]
    fn locate_clip_finds_global_and_track_local_indices() {
        // Layout: V1A, V2X, V1B, V2Y, V1C → V1=[A,B,C] V2=[X,Y]
        let (pl, ids) = build(&[(0, 10.0), (1, 5.0), (0, 20.0), (1, 8.0), (0, 15.0)]);

        assert_eq!(locate_clip(&pl, ids[0]), Some((0, 0, 0)), "V1A");
        assert_eq!(locate_clip(&pl, ids[1]), Some((1, 0, 1)), "V2X");
        assert_eq!(locate_clip(&pl, ids[2]), Some((2, 1, 0)), "V1B");
        assert_eq!(locate_clip(&pl, ids[3]), Some((3, 1, 1)), "V2Y");
        assert_eq!(locate_clip(&pl, ids[4]), Some((4, 2, 0)), "V1C");
    }

    #[test]
    fn locate_clip_missing_returns_none() {
        let (pl, _) = build(&[(0, 10.0)]);
        assert_eq!(locate_clip(&pl, Uuid::nil()), None);
    }

    // --- gap_positions_in_track ---------------------------------------------

    #[test]
    fn gap_positions_only_account_for_clips_on_the_same_track() {
        // V1 has two 10s clips; V2 has a much longer 100s clip wedged
        // between them. The V1 gap math must IGNORE the V2 clip entirely.
        let (pl, _) = build(&[(0, 10.0), (1, 100.0), (0, 10.0)]);
        let gaps_v1 = gap_positions_in_track(&pl, 6.0, 0);

        // 10s @ 6 px/s clamps to CARD_MIN_PX (120 px) instead of 60 px.
        // gap0 = 3 (padding); gap1 = 3 + 120 + 1 = 124; gap2 = 124 + 120 + 1 = 245.
        assert_eq!(gaps_v1.len(), 3);
        assert!((gaps_v1[0] - 3.0).abs() < 0.001);
        assert!((gaps_v1[1] - 124.0).abs() < 0.001);
        assert!((gaps_v1[2] - 245.0).abs() < 0.001);

        let gaps_v2 = gap_positions_in_track(&pl, 6.0, 1);
        // V2 has one 100s clip @ 6 px/s = 600 px (within max).
        assert_eq!(gaps_v2.len(), 2);
        assert!((gaps_v2[0] - 3.0).abs() < 0.001);
        assert!((gaps_v2[1] - 604.0).abs() < 0.001);
    }

    // --- drop_target_for_clip: same-track reorder ----------------------------

    #[test]
    fn drop_no_movement_is_noop() {
        let (pl, ids) = build(&[(0, 10.0), (0, 20.0), (0, 30.0)]);
        let out = drop_target_for_clip(&pl, 6.0, ids[1], 0.0).unwrap();
        assert!(out.reorder.is_none(), "zero delta on V1B should be no-op");
    }

    #[test]
    fn drop_far_right_moves_to_end_of_track() {
        let (pl, ids) = build(&[(0, 10.0), (0, 20.0), (0, 30.0)]);
        // Drag V1A (index 0) far right.
        let out = drop_target_for_clip(&pl, 6.0, ids[0], 10_000.0).unwrap();
        assert_eq!(out.local_gap, 3, "snap to trailing gap of V1 lane");
        let (from, to) = out.reorder.expect("should reorder");
        assert_eq!(from, 0);
        // Single-track case: insert at the post-remove end (len-1 = 2).
        assert_eq!(to, 2);
    }

    #[test]
    fn drop_far_left_moves_to_start_of_track() {
        let (pl, ids) = build(&[(0, 10.0), (0, 20.0), (0, 30.0)]);
        // Drag V1C (index 2) far left.
        let out = drop_target_for_clip(&pl, 6.0, ids[2], -10_000.0).unwrap();
        assert_eq!(out.local_gap, 0);
        let (from, to) = out.reorder.expect("should reorder");
        assert_eq!(from, 2);
        assert_eq!(to, 0);
    }

    // --- drop_target_for_clip: MULTI-track lanes (the regressions) -----------

    #[test]
    fn drop_in_v2_does_not_touch_v1_clips() {
        // Layout: V1A(10), V2X(5), V1B(20), V2Y(8), V1C(15)
        let (pl, ids) = build(&[(0, 10.0), (1, 5.0), (0, 20.0), (1, 8.0), (0, 15.0)]);
        // Drag V2X (global idx 1, local 0) far right inside V2.
        let out = drop_target_for_clip(&pl, 6.0, ids[1], 10_000.0).unwrap();
        assert_eq!(out.local_gap, 2, "V2 has 2 lanes-gaps after end");
        let (from, to) = out.reorder.unwrap();
        assert_eq!(from, 1, "X is at global index 1");
        // Post-remove playlist: [V1A, V1B, V2Y, V1C].
        // We want V2X to end up AFTER V2Y → global insertion before V1C (idx 3)
        // OR at end (idx 3). Either way, result must put V2X after V2Y.
        let mut copy = pl.clone();
        copy.reorder(from, to).unwrap();
        let v2_order: Vec<Uuid> = copy.clips().iter().filter(|c| c.video_track == 1).map(|c| c.id).collect();
        assert_eq!(v2_order, vec![ids[3], ids[1]], "V2 should now be [Y, X]");
        let v1_order: Vec<Uuid> = copy.clips().iter().filter(|c| c.video_track == 0).map(|c| c.id).collect();
        assert_eq!(v1_order, vec![ids[0], ids[2], ids[4]], "V1 order MUST be preserved");
    }

    #[test]
    fn drop_v1_to_start_does_not_disturb_v2_order() {
        let (pl, ids) = build(&[(0, 10.0), (1, 5.0), (0, 20.0), (1, 8.0), (0, 15.0)]);
        // Drag V1C (global idx 4, local 2) all the way left.
        let out = drop_target_for_clip(&pl, 6.0, ids[4], -10_000.0).unwrap();
        let (from, to) = out.reorder.unwrap();
        assert_eq!(from, 4);
        let mut copy = pl.clone();
        copy.reorder(from, to).unwrap();
        let v1_order: Vec<Uuid> = copy.clips().iter().filter(|c| c.video_track == 0).map(|c| c.id).collect();
        assert_eq!(v1_order, vec![ids[4], ids[0], ids[2]], "V1 should now be [C, A, B]");
        let v2_order: Vec<Uuid> = copy.clips().iter().filter(|c| c.video_track == 1).map(|c| c.id).collect();
        assert_eq!(v2_order, vec![ids[1], ids[3]], "V2 order untouched");
    }

    #[test]
    fn drop_v1_middle_in_multi_track_layout() {
        // V1A(10), V2X(5), V1B(20), V2Y(8), V1C(15)
        let (pl, ids) = build(&[(0, 10.0), (1, 5.0), (0, 20.0), (1, 8.0), (0, 15.0)]);
        // Drag V1A across V1B's center → should snap to between B and C
        // (V1 local gap 2). Card widths at 6 px/s all clamp to 120 px;
        // moving the center 1 full card right is ~121 px.
        let out = drop_target_for_clip(&pl, 6.0, ids[0], 240.0).unwrap();
        assert_eq!(out.local_gap, 2, "snap between V1B and V1C");
        let (from, to) = out.reorder.unwrap();
        let mut copy = pl.clone();
        copy.reorder(from, to).unwrap();
        let v1_order: Vec<Uuid> = copy.clips().iter().filter(|c| c.video_track == 0).map(|c| c.id).collect();
        assert_eq!(v1_order, vec![ids[2], ids[0], ids[4]], "V1 should now be [B, A, C]");
    }

    #[test]
    fn drop_a1_only_reorders_audio_lane() {
        // V1 and A1 both populated. Drag A1's last clip far left.
        let (pl, ids) = build(&[(0, 10.0), (2, 4.0), (0, 12.0), (2, 6.0), (2, 8.0)]);
        // A1 = [a1, a2, a3] (ids[1], ids[3], ids[4]).
        let out = drop_target_for_clip(&pl, 6.0, ids[4], -10_000.0).unwrap();
        let (from, to) = out.reorder.unwrap();
        assert_eq!(from, 4);
        let mut copy = pl.clone();
        copy.reorder(from, to).unwrap();
        let a1_order: Vec<Uuid> = copy.clips().iter().filter(|c| c.video_track == 2).map(|c| c.id).collect();
        assert_eq!(a1_order, vec![ids[4], ids[1], ids[3]]);
        let v1_order: Vec<Uuid> = copy.clips().iter().filter(|c| c.video_track == 0).map(|c| c.id).collect();
        assert_eq!(v1_order, vec![ids[0], ids[2]]);
    }

    #[test]
    fn drop_indicator_x_uses_lane_local_gap_position() {
        let (pl, ids) = build(&[(0, 10.0), (1, 100.0), (0, 10.0)]);
        // Drag V1B left, expecting indicator at V1 gap 0 (= 3 px).
        let out = drop_target_for_clip(&pl, 6.0, ids[2], -10_000.0).unwrap();
        assert!(
            (out.indicator_x_px - 3.0).abs() < 0.001,
            "indicator must be lane-local, not influenced by the V2 clip",
        );
    }

    #[test]
    fn drop_one_clip_lane_is_always_noop() {
        let (pl, ids) = build(&[(0, 10.0), (1, 5.0)]);
        // V2 has a single clip — anywhere it's "dragged", it stays.
        let out = drop_target_for_clip(&pl, 6.0, ids[1], 500.0).unwrap();
        assert!(out.reorder.is_none());
        let out2 = drop_target_for_clip(&pl, 6.0, ids[1], -500.0).unwrap();
        assert!(out2.reorder.is_none());
    }

    #[test]
    fn drop_to_just_past_self_is_noop() {
        // Source at local index 1: gap 1 (left edge) and gap 2 (right edge)
        // both bracket its own slot → must be a no-op.
        let (pl, ids) = build(&[(0, 10.0), (0, 10.0), (0, 10.0)]);
        // 60 px = roughly half a clamped card → still within source's own gap.
        let out = drop_target_for_clip(&pl, 6.0, ids[1], 60.0).unwrap();
        assert!(out.reorder.is_none());
        let out2 = drop_target_for_clip(&pl, 6.0, ids[1], -60.0).unwrap();
        assert!(out2.reorder.is_none());
    }

    #[test]
    fn drop_unknown_clip_returns_none() {
        let (pl, _) = build(&[(0, 10.0)]);
        assert!(drop_target_for_clip(&pl, 6.0, Uuid::nil(), 100.0).is_none());
    }
}
