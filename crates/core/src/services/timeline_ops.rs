//! Single-track timeline operations: split, trim, ripple-delete.
//!
//! All functions are pure transforms over `&mut Playlist`. Validation
//! errors surface as `CoreError::InvalidTrim` / `ClipNotFound`. No I/O.

use std::time::Duration;

use crate::domain::{Clip, ClipId, Playlist};
use crate::errors::{CoreError, CoreResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrimSide {
    Left,
    Right,
}

/// Split the clip identified by `id` at local-time `local_us` (microseconds
/// from the clip's effective start, i.e. relative to `trim_in`). The
/// original clip becomes the left half; a new clip with a fresh UUID is
/// inserted directly after it as the right half.
pub fn split_at(playlist: &mut Playlist, id: ClipId, local_us: i64) -> CoreResult<()> {
    let idx = find_index(playlist, id)?;
    let split_abs_us = {
        let clip = &playlist.clips()[idx];
        let trim_in_us = clip.trim_in_us();
        let absolute = trim_in_us + local_us;
        // Reject splits at or beyond the edges (zero-length halves are useless).
        if local_us <= 0 || absolute >= clip.trim_out_us() {
            return Err(CoreError::InvalidTrim(format!(
                "split at {local_us}us outside trim window ({}..{})",
                trim_in_us,
                clip.trim_out_us()
            )));
        }
        absolute
    };

    let right_half: Clip = {
        let left = &mut playlist.clips_mut()[idx];
        let original_trim_out = left.trim_out;
        left.trim_out = Duration::from_micros(split_abs_us as u64);

        let asset_id = left.asset_id();
        let mut right = left.clone();
        right.id = uuid::Uuid::new_v4();
        right.trim_in = Duration::from_micros(split_abs_us as u64);
        right.trim_out = original_trim_out;
        // Thumbnails/waveform live on disk under the original clip's id; the
        // right half has a fresh id, so point it back at the source asset so
        // its preview imagery resolves instead of coming up blank.
        right.thumbnails = left.thumbnails.clone();
        right.origin_id = Some(asset_id);
        // The right half is inserted directly after the left on the same
        // track, so it must pack against the (now shorter) left rather than
        // inherit the left's absolute pin — otherwise a split on a dragged
        // clip would stack both halves at the same start. `None` packs it
        // flush after the left half.
        right.start_us = None;
        right
    };

    playlist.insert(idx + 1, right_half)?;
    Ok(())
}

/// Adjust a trim handle. `new_local_us` is measured from the *source* start
/// (not from the effective start). Clamps to the legal range so a UI drag
/// gone wild can never invert the window.
pub fn trim(
    playlist: &mut Playlist,
    id: ClipId,
    side: TrimSide,
    new_source_us: i64,
) -> CoreResult<()> {
    let idx = find_index(playlist, id)?;
    let clip = &mut playlist.clips_mut()[idx];
    let source_us = clip.source_duration_us();

    match side {
        TrimSide::Left => {
            // Must leave ≥ 100ms of playable window.
            let max = (clip.trim_out_us() - 100_000).max(0);
            let new = new_source_us.clamp(0, max);
            clip.trim_in = Duration::from_micros(new as u64);
        }
        TrimSide::Right => {
            let min = (clip.trim_in_us() + 100_000).min(source_us);
            let new = new_source_us.clamp(min, source_us);
            clip.trim_out = Duration::from_micros(new as u64);
        }
    }
    Ok(())
}

/// Ripple-delete: remove the clip and let subsequent clips slide left. For
/// the single-track concat model this is identical to plain `remove` since
/// there are no inter-clip gaps in our timeline representation.
pub fn ripple_delete(playlist: &mut Playlist, id: ClipId) -> CoreResult<Clip> {
    playlist.remove(id)
}

fn find_index(playlist: &Playlist, id: ClipId) -> CoreResult<usize> {
    playlist
        .clips()
        .iter()
        .position(|c| c.id == id)
        .ok_or(CoreError::ClipNotFound(id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{CodecProfile, MediaInfo, Resolution};
    use std::path::PathBuf;

    fn clip(secs: f32) -> Clip {
        Clip::new(
            PathBuf::from(format!("/tmp/{}.mp4", secs)),
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

    #[test]
    fn split_creates_two_halves() {
        let mut pl = Playlist::new();
        pl.push(clip(10.0));
        let id = pl.clips()[0].id;
        split_at(&mut pl, id, 4_000_000).unwrap();
        assert_eq!(pl.len(), 2);
        assert_eq!(pl.clips()[0].effective_duration_us(), 4_000_000);
        assert_eq!(pl.clips()[1].effective_duration_us(), 6_000_000);
        // Halves point at the same source file.
        assert_eq!(pl.clips()[0].path, pl.clips()[1].path);
        // Right half has a new id.
        assert_ne!(pl.clips()[0].id, pl.clips()[1].id);
    }

    #[test]
    fn split_on_pinned_clip_does_not_inherit_its_pin() {
        // A clip dragged to an absolute position (start_us = Some) must not
        // pass that pin to its right half, or both halves would stack.
        let mut pl = Playlist::new();
        let mut c = clip(10.0);
        c.start_us = Some(20_000_000);
        pl.push(c);
        let id = pl.clips()[0].id;
        split_at(&mut pl, id, 4_000_000).unwrap();
        assert_eq!(pl.clips()[0].start_us, Some(20_000_000), "left keeps its pin");
        assert_eq!(pl.clips()[1].start_us, None, "right packs after left, no pin");
    }

    #[test]
    fn split_right_half_points_at_source_assets() {
        // Thumbnails/waveform live on disk under the original clip's id; the
        // new right half must resolve to that same asset id, not its own new id.
        let mut pl = Playlist::new();
        pl.push(clip(10.0));
        let orig = pl.clips()[0].id;
        split_at(&mut pl, orig, 4_000_000).unwrap();
        let right = &pl.clips()[1];
        assert_ne!(right.id, orig, "right has a fresh id");
        assert_eq!(right.origin_id, Some(orig));
        assert_eq!(right.asset_id(), orig, "right resolves the original's assets");
        // Splitting the right half again must keep pointing at the original.
        let right_id = right.id;
        split_at(&mut pl, right_id, 2_000_000).unwrap();
        assert_eq!(pl.clips()[2].asset_id(), orig, "asset id survives a second split");
    }

    #[test]
    fn split_at_edge_rejected() {
        let mut pl = Playlist::new();
        pl.push(clip(10.0));
        let id = pl.clips()[0].id;
        assert!(split_at(&mut pl, id, 0).is_err());
        assert!(split_at(&mut pl, id, 10_000_000).is_err());
    }

    #[test]
    fn trim_left_clamps_to_minimum_window() {
        let mut pl = Playlist::new();
        pl.push(clip(5.0));
        let id = pl.clips()[0].id;
        // Asking to trim_in past trim_out should clamp.
        trim(&mut pl, id, TrimSide::Left, 99_000_000).unwrap();
        let c = &pl.clips()[0];
        assert!(c.effective_duration_us() >= 100_000);
        assert!(c.trim_in_us() <= c.trim_out_us() - 100_000);
    }

    #[test]
    fn trim_right_shortens() {
        let mut pl = Playlist::new();
        pl.push(clip(10.0));
        let id = pl.clips()[0].id;
        trim(&mut pl, id, TrimSide::Right, 6_000_000).unwrap();
        assert_eq!(pl.clips()[0].effective_duration_us(), 6_000_000);
        assert!(pl.clips()[0].is_trimmed());
    }

    #[test]
    fn ripple_delete_removes() {
        let mut pl = Playlist::new();
        pl.push(clip(5.0));
        pl.push(clip(7.0));
        let id = pl.clips()[0].id;
        ripple_delete(&mut pl, id).unwrap();
        assert_eq!(pl.len(), 1);
    }
}
