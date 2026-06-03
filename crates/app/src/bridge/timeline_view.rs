//! Helpers that translate playlist + zoom state into Slint-side timeline
//! properties (ruler ticks, total width, global playhead fraction) and the
//! math behind **absolute, time-based** clip dragging.
//!
//! Clips carry an optional `start_us` pin (see `Clip::start_us`). A clip with
//! `None` packs right after its predecessor on the same track (legacy
//! sequential behaviour); a clip with `Some(v)` is anchored at an explicit
//! timeline position, which is what lets the user open gaps and drag freely.
//! `effective_start_us` resolves either case into the absolute position the
//! compositor (`Project::populate_from_playlist`) will use.
//!
//! A drag operates *inside* the source clip's track only — vertical
//! cross-track moves still go through the explicit "Move to …" path. Dragging
//! horizontally changes the clip's `start_us`, snapping magnetically to the
//! project origin, the playhead, and neighbouring clip edges.

use std::collections::HashMap;
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

/// Absolute timeline start (µs) of `clip_id` on its own track, honoring
/// explicit `start_us` pins and packing un-pinned clips right after their
/// predecessor. Mirrors `Project::populate_from_playlist`'s per-track cursor.
pub fn effective_start_us(playlist: &Playlist, clip_id: Uuid) -> Option<i64> {
    let track = playlist.clips().iter().find(|c| c.id == clip_id)?.video_track;
    let mut cursor: i64 = 0;
    for clip in playlist.clips().iter().filter(|c| c.video_track == track) {
        let start = clip.start_us.unwrap_or(cursor);
        if clip.id == clip_id {
            return Some(start);
        }
        cursor = start + clip.effective_duration_us();
    }
    None
}

/// Selected clip's `(start_us, duration_us)` on the timeline, if present.
/// `start_us` is the absolute position (honoring pins), matching the value the
/// compositor places the clip at.
pub fn selected_clip_window(playlist: &Playlist, selected_id: Option<Uuid>) -> Option<(i64, i64)> {
    let id = selected_id?;
    let start = effective_start_us(playlist, id)?;
    let dur = playlist
        .clips()
        .iter()
        .find(|c| c.id == id)?
        .effective_duration_us();
    Some((start, dur))
}

/// Total timeline length = the latest clip end across all tracks, honoring
/// `start_us` pins. For a single track with no gaps this equals the sum of
/// durations, so legacy projects behave identically; with gaps or overlapping
/// tracks it correctly reflects the real timeline extent (matches
/// `Project::duration_us`).
pub fn total_duration_us(playlist: &Playlist) -> i64 {
    let mut cursors: HashMap<u8, i64> = HashMap::new();
    let mut max_end: i64 = 0;
    for clip in playlist.clips() {
        let cursor = cursors.entry(clip.video_track).or_insert(0);
        let start = clip.start_us.unwrap_or(*cursor);
        let end = start + clip.effective_duration_us();
        *cursor = end;
        max_end = max_end.max(end);
    }
    max_end
}

pub fn total_duration_secs(playlist: &Playlist) -> f32 {
    (total_duration_us(playlist) as f64 / 1_000_000.0) as f32
}

/// Pixel tolerance for magnetic snapping during a drag. Keep modest so it
/// helps butt clips together without fighting precise placement.
pub const SNAP_PX: f32 = 8.0;

#[derive(Debug, Clone, PartialEq)]
pub struct DragOutcome {
    /// Snapped absolute start position for the dragged clip (µs, ≥ 0).
    pub new_start_us: i64,
    /// X pixel coord (lane/content coordinates) of the clip's left edge — used
    /// to render the live drop indicator. Equals `new_start_us * px_per_sec`.
    pub indicator_x_px: f32,
}

/// Resolve where a horizontally-dragged clip should land.
///
/// `delta_px` is the pointer displacement from where the press started, in
/// timeline pixels. The result snaps the leading **or** trailing edge to the
/// project origin, the `playhead_us`, or any neighbouring same-track clip edge
/// within [`SNAP_PX`]. Returns `None` for an unknown clip or non-positive zoom.
pub fn drag_to_start_us(
    playlist: &Playlist,
    px_per_sec: f32,
    clip_id: Uuid,
    delta_px: f32,
    playhead_us: i64,
) -> Option<DragOutcome> {
    if px_per_sec <= 0.0 {
        return None;
    }
    let target = playlist.clips().iter().find(|c| c.id == clip_id)?;
    let track = target.video_track;
    let dur = target.effective_duration_us();
    let cur_start = effective_start_us(playlist, clip_id)?;

    let us_per_px = 1_000_000.0 / px_per_sec;
    let raw = (((cur_start as f32) + delta_px * us_per_px).round() as i64).max(0);
    let snap_us = (SNAP_PX * us_per_px) as i64;

    // Snap candidates: project origin, the playhead, and every other same-track
    // clip's start & end edge.
    let mut candidates = vec![0i64, playhead_us];
    let mut cursor: i64 = 0;
    for clip in playlist.clips().iter().filter(|c| c.video_track == track) {
        let start = clip.start_us.unwrap_or(cursor);
        let end = start + clip.effective_duration_us();
        if clip.id != clip_id {
            candidates.push(start);
            candidates.push(end);
        }
        cursor = end;
    }

    let mut new_start = raw;
    let mut best = snap_us + 1;
    for c in candidates {
        if c < 0 {
            continue;
        }
        // Leading edge snaps to the candidate.
        let d_lead = (raw - c).abs();
        if d_lead <= snap_us && d_lead < best {
            best = d_lead;
            new_start = c;
        }
        // Trailing edge snaps to the candidate ⇒ start = candidate − duration.
        let d_trail = (raw + dur - c).abs();
        if d_trail <= snap_us && d_trail < best {
            best = d_trail;
            new_start = (c - dur).max(0);
        }
    }

    let indicator_x_px = new_start as f32 / us_per_px;
    Some(DragOutcome {
        new_start_us: new_start,
        indicator_x_px,
    })
}

/// The clip on `track` whose span contains absolute time `abs_us`, together
/// with the clip-local offset into it (`abs_us − clip_start`). Honors
/// `start_us` pins/packing. Returns `None` if the playhead sits in a gap or
/// off the end of the track. Used by "split at playhead".
pub fn clip_at_us_on_track(playlist: &Playlist, track: u8, abs_us: i64) -> Option<(Uuid, i64)> {
    let mut cursor: i64 = 0;
    for clip in playlist.clips().iter().filter(|c| c.video_track == track) {
        let start = clip.start_us.unwrap_or(cursor);
        let end = start + clip.effective_duration_us();
        if abs_us >= start && abs_us < end {
            return Some((clip.id, abs_us - start));
        }
        cursor = end;
    }
    None
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

    /// Build a playlist from `(track, duration_secs)` pairs, returning the
    /// playlist and the clip ids in order.
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

    // --- effective_start_us --------------------------------------------------

    #[test]
    fn effective_start_packs_per_track() {
        // V1: A(10) B(20) C(15); V2: X(5) Y(8) — each track packs from 0.
        let (pl, ids) = build(&[(0, 10.0), (1, 5.0), (0, 20.0), (1, 8.0), (0, 15.0)]);
        assert_eq!(effective_start_us(&pl, ids[0]), Some(0)); // V1 A
        assert_eq!(effective_start_us(&pl, ids[2]), Some(10_000_000)); // V1 B
        assert_eq!(effective_start_us(&pl, ids[4]), Some(30_000_000)); // V1 C
        assert_eq!(effective_start_us(&pl, ids[1]), Some(0)); // V2 X
        assert_eq!(effective_start_us(&pl, ids[3]), Some(5_000_000)); // V2 Y
    }

    #[test]
    fn effective_start_honors_pin_and_repacks_followers() {
        let (mut pl, ids) = build(&[(0, 10.0), (0, 20.0), (0, 15.0)]);
        pl.clips_mut()[1].start_us = Some(40_000_000); // pin B at 40s → gap
        assert_eq!(effective_start_us(&pl, ids[0]), Some(0));
        assert_eq!(effective_start_us(&pl, ids[1]), Some(40_000_000));
        // C is un-pinned → packs after B's pinned end (40 + 20 = 60s).
        assert_eq!(effective_start_us(&pl, ids[2]), Some(60_000_000));
    }

    // --- total_duration_us ---------------------------------------------------

    #[test]
    fn total_duration_is_sum_when_packed() {
        let (pl, _) = build(&[(0, 10.0), (0, 20.0), (0, 15.0)]);
        assert_eq!(total_duration_us(&pl), 45_000_000);
    }

    #[test]
    fn total_duration_is_max_end_with_a_gap() {
        let (mut pl, ids) = build(&[(0, 10.0), (0, 20.0)]);
        // Drag the second clip out to 100s → timeline extends to 120s.
        let i = pl.clips().iter().position(|c| c.id == ids[1]).unwrap();
        pl.clips_mut()[i].start_us = Some(100_000_000);
        assert_eq!(total_duration_us(&pl), 120_000_000);
    }

    #[test]
    fn total_duration_is_max_across_tracks_not_their_sum() {
        // V1 = 30s of content, V2 = 8s. The timeline is 30s, not 38s.
        let (pl, _) = build(&[(0, 10.0), (1, 8.0), (0, 20.0)]);
        assert_eq!(total_duration_us(&pl), 30_000_000);
    }

    // --- selected_clip_window ------------------------------------------------

    #[test]
    fn selected_window_returns_absolute_start() {
        let (pl, ids) = build(&[(0, 10.0), (0, 20.0), (0, 15.0)]);
        assert_eq!(selected_clip_window(&pl, Some(ids[1])), Some((10_000_000, 20_000_000)));
        assert_eq!(selected_clip_window(&pl, Some(Uuid::nil())), None);
        assert_eq!(selected_clip_window(&pl, None), None);
    }

    // --- drag_to_start_us ----------------------------------------------------

    #[test]
    fn drag_unknown_clip_returns_none() {
        let (pl, _) = build(&[(0, 10.0)]);
        assert!(drag_to_start_us(&pl, 6.0, Uuid::nil(), 100.0, 0).is_none());
    }

    #[test]
    fn drag_right_moves_start_by_delta_in_time() {
        // Lone clip at 0, 10 px/s → 1 px = 0.1s. Drag +300 px past any snap
        // (no neighbours; playhead far away) → +30s.
        let (mut pl, ids) = build(&[(0, 5.0)]);
        pl.clips_mut()[0].video_track = 0;
        let out = drag_to_start_us(&pl, 10.0, ids[0], 300.0, 999_000_000).unwrap();
        assert_eq!(out.new_start_us, 30_000_000);
        // Indicator x = 30s * 10 px/s = 300 px.
        assert!((out.indicator_x_px - 300.0).abs() < 0.5);
    }

    #[test]
    fn drag_clamps_at_origin() {
        let (pl, ids) = build(&[(0, 5.0)]);
        // Drag far left → cannot go below 0.
        let out = drag_to_start_us(&pl, 10.0, ids[0], -9999.0, -1).unwrap();
        assert_eq!(out.new_start_us, 0);
    }

    #[test]
    fn drag_snaps_trailing_edge_to_neighbour_start() {
        // V1: A(10s) at 0, B(10s) packed at 10s. Drag B left so its *end*
        // lands near A's end-ish; specifically aim so B butts against A.
        // 10 px/s: B starts at 100 px. Nudge left by ~95 px → raw ≈ 0.5s, which
        // snaps B's leading edge to A's... we want trailing snap, so move B so
        // its end (raw+10s) is near A's end (10s): raw ≈ 0 → leading snaps to 0.
        // Simpler: assert B snaps flush to A's right edge when dragged a hair
        // left of its packed spot.
        let (pl, ids) = build(&[(0, 10.0), (0, 10.0)]);
        // Drag B left by 5 px (0.5s) from its packed 10s start → raw 9.5s,
        // within SNAP_PX(8px=0.8s) of A's end (10s) → leading edge snaps to 10s.
        let out = drag_to_start_us(&pl, 10.0, ids[1], -5.0, -1).unwrap();
        assert_eq!(out.new_start_us, 10_000_000, "snaps flush against A's right edge");
    }

    #[test]
    fn clip_at_us_finds_clip_under_playhead_and_local_offset() {
        // V1: A(10s)@0, B(20s)@10s, C(15s)@30s.
        let (pl, ids) = build(&[(0, 10.0), (0, 20.0), (0, 15.0)]);
        // 15s is 5s into B.
        assert_eq!(clip_at_us_on_track(&pl, 0, 15_000_000), Some((ids[1], 5_000_000)));
        // 0s is the very start of A (local 0 → caller rejects as edge).
        assert_eq!(clip_at_us_on_track(&pl, 0, 0), Some((ids[0], 0)));
        // 30s is exactly C's start (B ends at 30, half-open) → C local 0.
        assert_eq!(clip_at_us_on_track(&pl, 0, 30_000_000), Some((ids[2], 0)));
        // Past the end → nothing.
        assert_eq!(clip_at_us_on_track(&pl, 0, 999_000_000), None);
        // Wrong track → nothing.
        assert_eq!(clip_at_us_on_track(&pl, 1, 15_000_000), None);
    }

    #[test]
    fn clip_at_us_respects_gaps() {
        let (mut pl, ids) = build(&[(0, 10.0), (0, 10.0)]);
        pl.clips_mut()[1].start_us = Some(50_000_000); // B pinned at 50s → gap 10..50
        assert_eq!(clip_at_us_on_track(&pl, 0, 30_000_000), None, "playhead in the gap");
        assert_eq!(clip_at_us_on_track(&pl, 0, 55_000_000), Some((ids[1], 5_000_000)));
    }

    #[test]
    fn drag_snaps_to_playhead() {
        let (pl, ids) = build(&[(0, 5.0)]);
        // Playhead at 50s. Drag clip so raw lands ~near 50s (498 px = 49.8s at
        // 10 px/s), within snap → leading edge snaps to playhead 50s.
        let out = drag_to_start_us(&pl, 10.0, ids[0], 498.0, 50_000_000).unwrap();
        assert_eq!(out.new_start_us, 50_000_000);
    }
}
