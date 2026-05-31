//! Multi-track timeline operations: split, trim, ripple-delete, move.
//!
//! Unlike the single-track [`timeline_ops`](super::timeline_ops) module,
//! these functions operate on a [`Project`] and are track-aware. Gaps
//! between clips on the same track are allowed — only overlaps are rejected.

use std::time::Duration;

use crate::domain::clip::{Clip, ClipId};
use crate::domain::project::{Project, TrackClip};
use crate::errors::{CoreError, CoreResult};

use super::timeline_ops::TrimSide;

/// Split the clip `id` at `local_us` microseconds from the clip's effective
/// start. The original becomes the left half; a new clip with a fresh UUID
/// is inserted immediately after it on the same track.
pub fn split_at_mt(project: &mut Project, clip_id: ClipId, local_us: i64) -> CoreResult<()> {
    let (ti, ci) = project
        .find_clip(clip_id)
        .ok_or(CoreError::ClipNotFound(clip_id))?;

    let split_abs_us = {
        let tc = &project.tracks[ti].clips[ci];
        let trim_in_us = tc.clip.trim_in_us();
        let absolute = trim_in_us + local_us;
        if local_us <= 0 || absolute >= tc.clip.trim_out_us() {
            return Err(CoreError::InvalidTrim(format!(
                "split at {local_us}µs outside trim window ({}..{})",
                trim_in_us,
                tc.clip.trim_out_us()
            )));
        }
        absolute
    };

    let right_tc = {
        let tc = &mut project.tracks[ti].clips[ci];
        let original_trim_out = tc.clip.trim_out;
        let left_new_duration_us = split_abs_us - tc.clip.trim_in_us();

        tc.clip.trim_out = Duration::from_micros(split_abs_us as u64);

        let mut right_clip = tc.clip.clone();
        right_clip.id = uuid::Uuid::new_v4();
        right_clip.trim_in = Duration::from_micros(split_abs_us as u64);
        right_clip.trim_out = original_trim_out;
        right_clip.thumbnails = tc.clip.thumbnails.clone();

        TrackClip {
            clip: right_clip,
            start_us: tc.start_us + left_new_duration_us,
        }
    };

    project.tracks[ti].clips.insert(ci + 1, right_tc);
    Ok(())
}

/// Adjust a trim handle on a clip in a multi-track project.
///
/// For left trims, the clip's `start_us` is also adjusted so the right
/// edge stays fixed. Clamps to ensure at least 100ms of playable content.
pub fn trim_mt(
    project: &mut Project,
    clip_id: ClipId,
    side: TrimSide,
    new_source_us: i64,
) -> CoreResult<()> {
    let (ti, ci) = project
        .find_clip(clip_id)
        .ok_or(CoreError::ClipNotFound(clip_id))?;

    let tc = &mut project.tracks[ti].clips[ci];
    let source_us = tc.clip.source_duration_us();

    match side {
        TrimSide::Left => {
            let max = (tc.clip.trim_out_us() - 100_000).max(0);
            let new = new_source_us.clamp(0, max);
            let old_trim_in_us = tc.clip.trim_in_us();
            tc.clip.trim_in = Duration::from_micros(new as u64);
            // Shift start_us so the right edge doesn't move.
            let delta = new - old_trim_in_us;
            tc.start_us += delta;
        }
        TrimSide::Right => {
            let min = (tc.clip.trim_in_us() + 100_000).min(source_us);
            let new = new_source_us.clamp(min, source_us);
            tc.clip.trim_out = Duration::from_micros(new as u64);
        }
    }
    Ok(())
}

/// Ripple-delete: remove the clip and slide all subsequent clips on the
/// **same track** left to close the gap.
pub fn ripple_delete_mt(project: &mut Project, clip_id: ClipId) -> CoreResult<Clip> {
    let (ti, ci) = project
        .find_clip(clip_id)
        .ok_or(CoreError::ClipNotFound(clip_id))?;

    let removed = project.tracks[ti].clips.remove(ci);
    let gap_us = removed.clip.effective_duration_us();

    // Slide subsequent clips left.
    for tc in project.tracks[ti].clips[ci..].iter_mut() {
        if tc.start_us >= removed.start_us {
            tc.start_us = (tc.start_us - gap_us).max(0);
        }
    }

    Ok(removed.clip)
}

/// Move a clip from one track to another (or reposition on the same track).
///
/// If the target position would overlap an existing clip on the destination
/// track, returns `InvalidTrim` error.
pub fn move_clip(
    project: &mut Project,
    clip_id: ClipId,
    to_track: usize,
    new_start_us: i64,
) -> CoreResult<()> {
    if to_track >= project.tracks.len() {
        return Err(CoreError::InvalidTrim(format!(
            "target track index {} out of range (have {} tracks)",
            to_track,
            project.tracks.len()
        )));
    }

    let (from_track, ci) = project
        .find_clip(clip_id)
        .ok_or(CoreError::ClipNotFound(clip_id))?;

    let dur_us = project.tracks[from_track].clips[ci]
        .clip
        .effective_duration_us();

    // Check for overlaps on the destination track (excluding self if same track).
    let exclude = if from_track == to_track {
        Some(clip_id)
    } else {
        None
    };
    if project.tracks[to_track].would_overlap(new_start_us, dur_us, exclude) {
        return Err(CoreError::InvalidTrim(
            "clip would overlap an existing clip on the target track".into(),
        ));
    }

    if from_track == to_track {
        // Same track: just reposition.
        project.tracks[from_track].clips[ci].start_us = new_start_us.max(0);
    } else {
        // Cross-track: remove from source, insert on destination.
        let mut tc = project.tracks[from_track].clips.remove(ci);
        tc.start_us = new_start_us.max(0);
        project.tracks[to_track].clips.push(tc);
        // Keep sorted by start_us.
        project.tracks[to_track]
            .clips
            .sort_by_key(|tc| tc.start_us);
    }
    Ok(())
}

/// Insert a clip onto a track at the given position.
///
/// Returns an error if the placement overlaps any existing clip.
pub fn insert_clip(
    project: &mut Project,
    track_idx: usize,
    clip: Clip,
    start_us: i64,
) -> CoreResult<()> {
    if track_idx >= project.tracks.len() {
        return Err(CoreError::InvalidTrim(format!(
            "track index {} out of range",
            track_idx
        )));
    }
    let dur_us = clip.effective_duration_us();
    if project.tracks[track_idx].would_overlap(start_us, dur_us, None) {
        return Err(CoreError::InvalidTrim(
            "clip would overlap an existing clip on this track".into(),
        ));
    }
    project.tracks[track_idx].clips.push(TrackClip {
        clip,
        start_us: start_us.max(0),
    });
    project.tracks[track_idx]
        .clips
        .sort_by_key(|tc| tc.start_us);
    Ok(())
}

/// Append a clip to the end of a track (after the last clip).
pub fn append_clip(project: &mut Project, track_idx: usize, clip: Clip) -> CoreResult<()> {
    if track_idx >= project.tracks.len() {
        return Err(CoreError::InvalidTrim(format!(
            "track index {} out of range",
            track_idx
        )));
    }
    let end = project.tracks[track_idx].duration_us();
    insert_clip(project, track_idx, clip, end)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{CodecProfile, MediaInfo, Resolution};
    use std::path::PathBuf;

    fn make_clip(secs: f32) -> Clip {
        Clip::new(
            PathBuf::from(format!("/tmp/{}.mp4", secs)),
            MediaInfo {
                duration: Duration::from_secs_f32(secs),
                profile: CodecProfile {
                    video_codec: "h264".into(),
                    audio_codec: "aac".into(),
                    resolution: Resolution {
                        width: 1920,
                        height: 1080,
                    },
                    frame_rate_mhz: 30_000,
                    pixel_format: "yuv420p".into(),
                },
            },
        )
    }

    fn one_clip_project(secs: f32) -> (Project, ClipId) {
        let mut proj = Project::with_default_tracks();
        let clip = make_clip(secs);
        let id = clip.id;
        let v1 = proj.track_index_by_name("V1").unwrap();
        proj.tracks[v1].clips.push(TrackClip {
            clip,
            start_us: 0,
        });
        (proj, id)
    }

    #[test]
    fn split_creates_two_halves() {
        let (mut proj, id) = one_clip_project(10.0);
        split_at_mt(&mut proj, id, 4_000_000).unwrap();
        let v1 = proj.track_index_by_name("V1").unwrap();
        assert_eq!(proj.tracks[v1].clips.len(), 2);
        assert_eq!(
            proj.tracks[v1].clips[0].clip.effective_duration_us(),
            4_000_000
        );
        assert_eq!(
            proj.tracks[v1].clips[1].clip.effective_duration_us(),
            6_000_000
        );
        assert_eq!(proj.tracks[v1].clips[1].start_us, 4_000_000);
    }

    #[test]
    fn trim_left_shifts_start() {
        let (mut proj, id) = one_clip_project(10.0);
        // Trim in by 2s.
        trim_mt(&mut proj, id, TrimSide::Left, 2_000_000).unwrap();
        let v1 = proj.track_index_by_name("V1").unwrap();
        let tc = &proj.tracks[v1].clips[0];
        assert_eq!(tc.clip.trim_in_us(), 2_000_000);
        assert_eq!(tc.start_us, 2_000_000);
    }

    #[test]
    fn ripple_delete_closes_gap() {
        let mut proj = Project::with_default_tracks();
        let v1 = proj.track_index_by_name("V1").unwrap();
        let c1 = make_clip(5.0);
        let c2 = make_clip(3.0);
        let c1_id = c1.id;
        proj.tracks[v1].clips.push(TrackClip {
            clip: c1,
            start_us: 0,
        });
        proj.tracks[v1].clips.push(TrackClip {
            clip: c2,
            start_us: 5_000_000,
        });

        ripple_delete_mt(&mut proj, c1_id).unwrap();
        assert_eq!(proj.tracks[v1].clips.len(), 1);
        assert_eq!(proj.tracks[v1].clips[0].start_us, 0);
    }

    #[test]
    fn move_across_tracks() {
        let (mut proj, id) = one_clip_project(5.0);
        let v2 = proj.track_index_by_name("V2").unwrap();
        move_clip(&mut proj, id, v2, 2_000_000).unwrap();
        let v1 = proj.track_index_by_name("V1").unwrap();
        assert!(proj.tracks[v1].clips.is_empty());
        assert_eq!(proj.tracks[v2].clips.len(), 1);
        assert_eq!(proj.tracks[v2].clips[0].start_us, 2_000_000);
    }

    #[test]
    fn overlap_rejected() {
        let mut proj = Project::with_default_tracks();
        let v1 = proj.track_index_by_name("V1").unwrap();
        let c1 = make_clip(5.0);
        proj.tracks[v1].clips.push(TrackClip {
            clip: c1,
            start_us: 0,
        });
        // Try to insert overlapping clip.
        let c2 = make_clip(3.0);
        let result = insert_clip(&mut proj, v1, c2, 3_000_000);
        assert!(result.is_err());
    }

    #[test]
    fn append_places_at_end() {
        let mut proj = Project::with_default_tracks();
        let v1 = proj.track_index_by_name("V1").unwrap();
        let c1 = make_clip(5.0);
        let c2 = make_clip(3.0);
        append_clip(&mut proj, v1, c1).unwrap();
        append_clip(&mut proj, v1, c2).unwrap();
        assert_eq!(proj.tracks[v1].clips.len(), 2);
        assert_eq!(proj.tracks[v1].clips[0].start_us, 0);
        assert_eq!(proj.tracks[v1].clips[1].start_us, 5_000_000);
    }
}
