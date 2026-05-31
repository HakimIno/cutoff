//! Helpers that translate playlist + zoom state into Slint-side timeline
//! properties (ruler ticks, total width, global playhead fraction).

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

// Layout constants — keep in sync with timeline.slint `padding-left`
// (`Theme.spacing-xs` = 3px) and the inter-card `spacing: 1px`.
pub const LANE_PADDING_PX: f32 = 3.0;
pub const CARD_SPACING_PX: f32 = 1.0;

pub const CARD_MIN_PX: f32 = 120.0;
pub const CARD_MAX_PX: f32 = 800.0;

pub fn card_width(duration_secs: f32, px_per_sec: f32) -> f32 {
    (duration_secs * px_per_sec).clamp(CARD_MIN_PX, CARD_MAX_PX)
}

/// Cumulative left-edge positions for each insertion gap.
///
/// `gaps[i]` is the x position where a card inserted at index `i` would land.
/// `gaps.len() == clips.len() + 1`. Coordinates match the lane's content
/// origin (so padding is included).
pub fn gap_positions(playlist: &Playlist, px_per_sec: f32) -> Vec<f32> {
    let mut x = LANE_PADDING_PX;
    let mut gaps = Vec::with_capacity(playlist.clips().len() + 1);
    gaps.push(x);
    for clip in playlist.clips() {
        let w = card_width(clip.effective_duration().as_secs_f32(), px_per_sec);
        x += w + CARD_SPACING_PX;
        gaps.push(x);
    }
    gaps
}

/// Decide where a card currently being dragged should drop.
///
/// `from_idx`: original index of the dragged card.
/// `delta_px`: pointer x displacement from where the press started.
///
/// Returns `(target_gap_idx, indicator_x_px, reorder_to)`:
/// * `target_gap_idx` indexes into `gap_positions` (0..=N).
/// * `indicator_x_px` is the absolute x to render the drop indicator at.
/// * `reorder_to` is the `Playlist::reorder` `to` parameter, or `None` if
///   the drop is a no-op (own position).
pub fn drop_target(
    playlist: &Playlist,
    px_per_sec: f32,
    from_idx: usize,
    delta_px: f32,
) -> Option<(usize, f32, Option<usize>)> {
    let n = playlist.clips().len();
    if from_idx >= n {
        return None;
    }
    let gaps = gap_positions(playlist, px_per_sec);
    let widths: Vec<f32> = playlist
        .clips()
        .iter()
        .map(|c| card_width(c.effective_duration().as_secs_f32(), px_per_sec))
        .collect();

    let orig_center = gaps[from_idx] + widths[from_idx] / 2.0;
    let new_center = orig_center + delta_px;

    let mut best_idx = 0usize;
    let mut best_dist = f32::INFINITY;
    for (i, x) in gaps.iter().enumerate() {
        let d = (*x - new_center).abs();
        if d < best_dist {
            best_dist = d;
            best_idx = i;
        }
    }

    // Translate insertion gap → Playlist::reorder `to` index.
    // No-op when dropping at the original slot or the slot just after.
    let reorder_to = if best_idx == from_idx || best_idx == from_idx + 1 {
        None
    } else if best_idx > from_idx {
        Some(best_idx - 1)
    } else {
        Some(best_idx)
    };

    Some((best_idx, gaps[best_idx], reorder_to))
}

/// Map a fraction of the entire timeline (0..1) back to a clip + local pts.
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

    fn fake_clip(secs: f32) -> Clip {
        Clip::new(
            std::path::PathBuf::from(format!("/tmp/{}.mp4", secs)),
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
        )
    }

    fn playlist_of(durations: &[f32]) -> Playlist {
        let mut pl = Playlist::new();
        for &d in durations {
            pl.push(fake_clip(d));
        }
        pl
    }

    #[test]
    fn drop_no_movement_is_noop() {
        let pl = playlist_of(&[10.0, 20.0, 30.0]);
        let (_, _, reorder) = drop_target(&pl, 6.0, 1, 0.0).unwrap();
        assert!(reorder.is_none(), "zero delta should be no-op");
    }

    #[test]
    fn drop_far_right_moves_to_end() {
        let pl = playlist_of(&[10.0, 20.0, 30.0]);
        let (gap, _x, reorder) = drop_target(&pl, 6.0, 0, 10_000.0).unwrap();
        assert_eq!(gap, 3, "gap should be the trailing one");
        assert_eq!(reorder, Some(2), "moving clip 0 to end means reorder to last index");
    }

    #[test]
    fn drop_far_left_moves_to_start() {
        let pl = playlist_of(&[10.0, 20.0, 30.0]);
        let (gap, _x, reorder) = drop_target(&pl, 6.0, 2, -10_000.0).unwrap();
        assert_eq!(gap, 0);
        assert_eq!(reorder, Some(0));
    }
}
