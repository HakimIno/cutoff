use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;
use uuid::Uuid;

use super::codec::CodecProfile;

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
}

fn default_volume() -> f32 {
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
