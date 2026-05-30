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
