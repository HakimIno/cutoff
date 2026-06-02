//! Per-clip visual transform.
//!
//! [`ResolvedTransform`] is the "flattened" set of values that the preview
//! compositor and the export filter graph both consume to place a clip on the
//! canvas. It is produced by [`Clip::resolved_transform`](crate::domain::Clip::resolved_transform).
//!
//! In Phase 1 the resolved transform is just a copy of the clip's static
//! fields. Phase 2 (keyframe animation) will make `resolved_transform(local_us)`
//! evaluate animated tracks at `local_us` and return the interpolated values —
//! every renderer already calls it per frame, so animation will flow through
//! automatically once the evaluation changes.

use serde::{Deserialize, Serialize};

/// A rectangular crop applied to the *source* frame before scaling, expressed
/// as pixels trimmed off each edge in source-resolution space.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Crop {
    pub left: u32,
    pub top: u32,
    pub right: u32,
    pub bottom: u32,
}

impl Crop {
    /// Whether this crop removes anything at all.
    pub fn is_noop(&self) -> bool {
        self.left == 0 && self.top == 0 && self.right == 0 && self.bottom == 0
    }
}

/// Fully-resolved transform values at a single point in time. All renderers
/// (preview + export) read *only* this struct so they stay perfectly in sync.
///
/// Geometry convention shared by preview and export:
///   * the clip is scaled by `scale_x`/`scale_y` (optionally flipped),
///     rotated by `rotation_deg` around its own center, then translated by
///     `position_x`/`position_y` (full project-resolution pixels) relative to
///     a centered origin;
///   * `opacity` is a final alpha multiply in `0.0..=1.0`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ResolvedTransform {
    pub opacity: f32,
    pub scale_x: f32,
    pub scale_y: f32,
    pub position_x: i32,
    pub position_y: i32,
    pub rotation_deg: f32,
    pub flip_h: bool,
    pub flip_v: bool,
    pub crop: Option<Crop>,
}

impl Default for ResolvedTransform {
    fn default() -> Self {
        Self {
            opacity: 1.0,
            scale_x: 1.0,
            scale_y: 1.0,
            position_x: 0,
            position_y: 0,
            rotation_deg: 0.0,
            flip_h: false,
            flip_v: false,
            crop: None,
        }
    }
}

impl ResolvedTransform {
    /// True when the clip occupies the full canvas with no visual change —
    /// lets renderers take a zero-copy fast path.
    pub fn is_identity(&self) -> bool {
        self.opacity >= 1.0
            && (self.scale_x - 1.0).abs() < 1e-3
            && (self.scale_y - 1.0).abs() < 1e-3
            && self.position_x == 0
            && self.position_y == 0
            && self.rotation_deg.abs() < 1e-3
            && !self.flip_h
            && !self.flip_v
            && self.crop.map_or(true, |c| c.is_noop())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Clip, CodecProfile, MediaInfo, Resolution};
    use std::path::PathBuf;
    use std::time::Duration;

    fn clip() -> Clip {
        Clip::new(
            PathBuf::from("/tmp/a.mp4"),
            MediaInfo {
                duration: Duration::from_secs(5),
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
    fn fresh_clip_resolves_to_identity() {
        assert!(clip().resolved_transform(0).is_identity());
    }

    #[test]
    fn uniform_scale_multiplies_per_axis() {
        let mut c = clip();
        c.scale = 2.0;
        c.scale_x = 1.5;
        let r = c.resolved_transform(0);
        assert!((r.scale_x - 3.0).abs() < 1e-6, "uniform × per-axis");
        assert!((r.scale_y - 2.0).abs() < 1e-6);
        assert!(!r.is_identity());
    }

    #[test]
    fn noop_crop_is_dropped() {
        let mut c = clip();
        c.crop = Some(Crop { left: 0, top: 0, right: 0, bottom: 0 });
        assert_eq!(c.resolved_transform(0).crop, None);
    }
}
