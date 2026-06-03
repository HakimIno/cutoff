use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;
use uuid::Uuid;

use super::codec::CodecProfile;
use super::transform::{ColorGrading, Crop, ResolvedTransform, TransformKeys};

pub type ClipId = Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Clip {
    pub id: ClipId,
    pub path: PathBuf,
    pub info: MediaInfo,
    /// Trim-in offset from source start. Zero means "no left trim".
    #[serde(default)]
    pub trim_in: Duration,
    /// Trim-out absolute position from source start. `info.duration` means
    /// "no right trim". Always > trim_in.
    #[serde(default)]
    pub trim_out: Duration,
    /// Runtime-populated paths to extracted preview thumbnails (oldest→newest in time).
    /// Skipped from serde because they're regenerated on import.
    #[serde(default, skip)]
    pub thumbnails: Vec<PathBuf>,
    /// Per-clip linear volume multiplier. 1.0 = unity, 2.0 = +6dB, 0.0 = silent.
    #[serde(default = "default_volume")]
    pub volume: f32,
    /// Per-clip mute toggle.
    #[serde(default)]
    pub muted: bool,
    /// Cached waveform peaks path (runtime-only).
    #[serde(default, skip)]
    pub waveform_path: Option<PathBuf>,
    /// Which video track the clip lives on. 0 = V1 (base), 1 = V2 (overlay).
    /// Audio waveform follows the same lane. Defaults to V1 so v1 projects
    /// load cleanly without migration logic.
    #[serde(default)]
    pub video_track: u8,
    /// Opacity (0.0 to 1.0)
    #[serde(default = "default_opacity")]
    pub opacity: f32,
    /// Uniform scale (0.0 to 2.0). Kept for back-compat with v1 projects; the
    /// effective per-axis scale is `scale * scale_x` / `scale * scale_y`.
    #[serde(default = "default_scale")]
    pub scale: f32,
    /// X Position offset (pixels, full project-resolution space).
    #[serde(default)]
    pub position_x: i32,
    /// Y Position offset (pixels, full project-resolution space).
    #[serde(default)]
    pub position_y: i32,
    /// Per-axis scale multiplier (on top of the uniform `scale`). 1.0 = none.
    #[serde(default = "default_scale")]
    pub scale_x: f32,
    #[serde(default = "default_scale")]
    pub scale_y: f32,
    /// Clockwise rotation around the clip center, in degrees.
    #[serde(default)]
    pub rotation_deg: f32,
    /// Horizontal / vertical mirror flips.
    #[serde(default)]
    pub flip_h: bool,
    #[serde(default)]
    pub flip_v: bool,
    /// Optional source-space crop applied before scaling.
    #[serde(default)]
    pub crop: Option<Crop>,
    /// Per-channel keyframe animation. Empty by default → static transform.
    #[serde(default)]
    pub transform_keys: TransformKeys,
    /// Brightness correction (-1.0 to 1.0, default 0.0)
    #[serde(default = "default_brightness")]
    pub brightness: f32,
    /// Contrast correction (0.0 to 2.0, default 1.0)
    #[serde(default = "default_contrast_saturation")]
    pub contrast: f32,
    /// Saturation correction (0.0 to 2.0, default 1.0)
    #[serde(default = "default_contrast_saturation")]
    pub saturation: f32,
    /// Color grading parameters (Lift, Gamma, Gain, Temp, Tint, Vignette)
    #[serde(default)]
    pub color_grading: ColorGrading,
}

fn default_brightness() -> f32 {
    0.0
}

fn default_contrast_saturation() -> f32 {
    1.0
}

fn default_volume() -> f32 {
    1.0
}

fn default_opacity() -> f32 {
    1.0
}

fn default_scale() -> f32 {
    1.0
}

impl Clip {
    pub fn new(path: PathBuf, info: MediaInfo) -> Self {
        let trim_out = info.duration;
        Self {
            id: Uuid::new_v4(),
            path,
            info,
            trim_in: Duration::ZERO,
            trim_out,
            thumbnails: Vec::new(),
            volume: 1.0,
            muted: false,
            waveform_path: None,
            video_track: 0,
            opacity: 1.0,
            scale: 1.0,
            position_x: 0,
            position_y: 0,
            scale_x: 1.0,
            scale_y: 1.0,
            rotation_deg: 0.0,
            flip_h: false,
            flip_v: false,
            crop: None,
            transform_keys: TransformKeys::default(),
            brightness: 0.0,
            contrast: 1.0,
            saturation: 1.0,
            color_grading: ColorGrading::default(),
        }
    }

    /// Resolve this clip's transform at `rel_us` — microseconds since the
    /// clip's *visible start* on the timeline (`timeline_t - start_us`), the
    /// domain keyframes are authored in.
    ///
    /// Each channel evaluates its keyframe track, falling back to the static
    /// field when that channel has no keyframes. Flip and crop are not animated.
    pub fn resolved_transform(&self, rel_us: i64) -> ResolvedTransform {
        let k = &self.transform_keys;
        ResolvedTransform {
            opacity: k.opacity.eval(rel_us, self.opacity),
            scale_x: k.scale.eval(rel_us, self.scale) * k.scale_x.eval(rel_us, self.scale_x),
            scale_y: k.scale.eval(rel_us, self.scale) * k.scale_y.eval(rel_us, self.scale_y),
            position_x: k.position_x.eval(rel_us, self.position_x as f32).round() as i32,
            position_y: k.position_y.eval(rel_us, self.position_y as f32).round() as i32,
            rotation_deg: k.rotation_deg.eval(rel_us, self.rotation_deg),
            flip_h: self.flip_h,
            flip_v: self.flip_v,
            crop: self.crop.filter(|c| !c.is_noop()),
            brightness: self.brightness,
            contrast: self.contrast,
            saturation: self.saturation,
            color_grading: self.color_grading,
        }
    }

    pub fn is_compatible_with(&self, other: &Clip) -> bool {
        self.info.profile == other.info.profile
    }

    /// Effective playback duration after applying trim.
    pub fn effective_duration(&self) -> Duration {
        self.trim_out.saturating_sub(self.trim_in)
    }

    pub fn effective_duration_us(&self) -> i64 {
        self.effective_duration().as_micros() as i64
    }

    pub fn trim_in_us(&self) -> i64 {
        self.trim_in.as_micros() as i64
    }

    pub fn trim_out_us(&self) -> i64 {
        self.trim_out.as_micros() as i64
    }

    pub fn source_duration_us(&self) -> i64 {
        self.info.duration.as_micros() as i64
    }

    pub fn is_trimmed(&self) -> bool {
        self.trim_in > Duration::ZERO || self.trim_out < self.info.duration
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaInfo {
    pub duration: Duration,
    pub profile: CodecProfile,
}
