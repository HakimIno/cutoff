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

// ── Keyframe animation (Phase 2) ───────────────────────────────────────────

/// How the value is interpolated from this keyframe to the next.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Interp {
    /// Step: hold this value until the next keyframe.
    Hold,
    /// Straight line to the next keyframe's value.
    Linear,
}

impl Default for Interp {
    fn default() -> Self {
        Interp::Linear
    }
}

/// A single animation keyframe. `time_us` is **clip-relative** — microseconds
/// since the clip's visible start on the timeline (independent of trim/placement).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Keyframe {
    pub time_us: i64,
    pub value: f32,
    #[serde(default)]
    pub interp: Interp,
}

/// An animatable scalar: a (time-sorted) list of keyframes. Empty = not
/// animated, in which case callers fall back to the clip's static field.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AnimatedF32 {
    #[serde(default)]
    keys: Vec<Keyframe>,
}

impl AnimatedF32 {
    pub fn is_animated(&self) -> bool {
        !self.keys.is_empty()
    }

    pub fn keys(&self) -> &[Keyframe] {
        &self.keys
    }

    /// Evaluate at clip-relative `t_us`. Returns `fallback` when not animated.
    /// Before the first / after the last keyframe the value is held (clamped).
    pub fn eval(&self, t_us: i64, fallback: f32) -> f32 {
        match self.keys.as_slice() {
            [] => fallback,
            [only] => only.value,
            keys => {
                if t_us <= keys[0].time_us {
                    return keys[0].value;
                }
                let last = keys[keys.len() - 1];
                if t_us >= last.time_us {
                    return last.value;
                }
                for w in keys.windows(2) {
                    let (a, b) = (w[0], w[1]);
                    if t_us >= a.time_us && t_us <= b.time_us {
                        return match a.interp {
                            Interp::Hold => a.value,
                            Interp::Linear => {
                                let span = (b.time_us - a.time_us).max(1) as f32;
                                let f = (t_us - a.time_us) as f32 / span;
                                a.value + (b.value - a.value) * f
                            }
                        };
                    }
                }
                last.value
            }
        }
    }

    /// Insert a keyframe at `time_us` (replacing one within `tol_us`), keeping
    /// the list time-sorted.
    pub fn upsert(&mut self, time_us: i64, value: f32, tol_us: i64) {
        if let Some(k) = self
            .keys
            .iter_mut()
            .find(|k| (k.time_us - time_us).abs() <= tol_us)
        {
            k.value = value;
            return;
        }
        self.keys.push(Keyframe { time_us, value, interp: Interp::default() });
        self.keys.sort_by_key(|k| k.time_us);
    }

    /// Remove a keyframe within `tol_us` of `time_us`. Returns whether one was removed.
    pub fn remove_near(&mut self, time_us: i64, tol_us: i64) -> bool {
        let before = self.keys.len();
        self.keys.retain(|k| (k.time_us - time_us).abs() > tol_us);
        self.keys.len() != before
    }

    pub fn has_key_near(&self, time_us: i64, tol_us: i64) -> bool {
        self.keys.iter().any(|k| (k.time_us - time_us).abs() <= tol_us)
    }
}

/// Addressable transform channels — lets the UI/bridge toggle a keyframe on a
/// specific property without a match arm per call site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransformChannel {
    Opacity,
    Scale,
    ScaleX,
    ScaleY,
    PositionX,
    PositionY,
    Rotation,
}

impl TransformChannel {
    /// Stable integer id used to cross the Slint boundary.
    pub fn from_id(id: i32) -> Option<Self> {
        Some(match id {
            0 => Self::Opacity,
            1 => Self::Scale,
            2 => Self::ScaleX,
            3 => Self::ScaleY,
            4 => Self::PositionX,
            5 => Self::PositionY,
            6 => Self::Rotation,
            _ => return None,
        })
    }
}

/// Per-channel keyframe tracks. All empty by default → fully static clip.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TransformKeys {
    #[serde(default)]
    pub opacity: AnimatedF32,
    #[serde(default)]
    pub scale: AnimatedF32,
    #[serde(default)]
    pub scale_x: AnimatedF32,
    #[serde(default)]
    pub scale_y: AnimatedF32,
    #[serde(default)]
    pub position_x: AnimatedF32,
    #[serde(default)]
    pub position_y: AnimatedF32,
    #[serde(default)]
    pub rotation_deg: AnimatedF32,
}

impl TransformKeys {
    pub fn channel(&self, ch: TransformChannel) -> &AnimatedF32 {
        match ch {
            TransformChannel::Opacity => &self.opacity,
            TransformChannel::Scale => &self.scale,
            TransformChannel::ScaleX => &self.scale_x,
            TransformChannel::ScaleY => &self.scale_y,
            TransformChannel::PositionX => &self.position_x,
            TransformChannel::PositionY => &self.position_y,
            TransformChannel::Rotation => &self.rotation_deg,
        }
    }

    pub fn channel_mut(&mut self, ch: TransformChannel) -> &mut AnimatedF32 {
        match ch {
            TransformChannel::Opacity => &mut self.opacity,
            TransformChannel::Scale => &mut self.scale,
            TransformChannel::ScaleX => &mut self.scale_x,
            TransformChannel::ScaleY => &mut self.scale_y,
            TransformChannel::PositionX => &mut self.position_x,
            TransformChannel::PositionY => &mut self.position_y,
            TransformChannel::Rotation => &mut self.rotation_deg,
        }
    }

    /// True if any channel has at least one keyframe.
    pub fn is_animated(&self) -> bool {
        self.opacity.is_animated()
            || self.scale.is_animated()
            || self.scale_x.is_animated()
            || self.scale_y.is_animated()
            || self.position_x.is_animated()
            || self.position_y.is_animated()
            || self.rotation_deg.is_animated()
    }
}

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
    pub brightness: f32,
    pub contrast: f32,
    pub saturation: f32,
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
            brightness: 0.0,
            contrast: 1.0,
            saturation: 1.0,
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
            && self.brightness.abs() < 1e-3
            && (self.contrast - 1.0).abs() < 1e-3
            && (self.saturation - 1.0).abs() < 1e-3
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

    #[test]
    fn empty_track_uses_fallback() {
        let a = AnimatedF32::default();
        assert_eq!(a.eval(1000, 0.7), 0.7);
        assert!(!a.is_animated());
    }

    #[test]
    fn linear_interpolation_and_clamping() {
        let mut a = AnimatedF32::default();
        a.upsert(0, 0.0, 0);
        a.upsert(1_000_000, 1.0, 0);
        assert_eq!(a.eval(-500, 9.9), 0.0, "held before first key");
        assert!((a.eval(500_000, 9.9) - 0.5).abs() < 1e-6, "midpoint");
        assert_eq!(a.eval(2_000_000, 9.9), 1.0, "held after last key");
    }

    #[test]
    fn hold_interp_does_not_interpolate() {
        let mut a = AnimatedF32::default();
        a.upsert(0, 0.0, 0);
        a.upsert(1_000_000, 1.0, 0);
        // Force the first key to Hold.
        assert_eq!(a.keys().len(), 2);
        // Re-create with hold on first key.
        let a = AnimatedF32 {
            keys: vec![
                Keyframe { time_us: 0, value: 0.0, interp: Interp::Hold },
                Keyframe { time_us: 1_000_000, value: 1.0, interp: Interp::Linear },
            ],
        };
        assert_eq!(a.eval(500_000, 9.9), 0.0, "hold keeps the earlier value");
    }

    #[test]
    fn upsert_replaces_within_tolerance_and_remove_works() {
        let mut a = AnimatedF32::default();
        a.upsert(1_000_000, 0.2, 1000);
        a.upsert(1_000_500, 0.9, 1000); // within tol → replace, not add
        assert_eq!(a.keys().len(), 1);
        assert!((a.eval(1_000_000, 0.0) - 0.9).abs() < 1e-6);
        assert!(a.has_key_near(1_000_200, 1000));
        assert!(a.remove_near(1_000_000, 1000));
        assert!(!a.is_animated());
    }

    #[test]
    fn keyframes_drive_resolved_transform() {
        let mut c = clip();
        c.transform_keys.opacity.upsert(0, 0.0, 0);
        c.transform_keys.opacity.upsert(1_000_000, 1.0, 0);
        assert!((c.resolved_transform(500_000).opacity - 0.5).abs() < 1e-6);
        // Non-animated channels still use the static field.
        c.scale = 2.0;
        assert!((c.resolved_transform(500_000).scale_x - 2.0).abs() < 1e-6);
    }
}
