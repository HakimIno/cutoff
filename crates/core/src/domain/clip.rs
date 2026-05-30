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
    /// Runtime-populated paths to extracted preview thumbnails (oldest→newest in time).
    /// Skipped from serde because they're regenerated on import.
    #[serde(default, skip)]
    pub thumbnails: Vec<PathBuf>,
}

impl Clip {
    pub fn new(path: PathBuf, info: MediaInfo) -> Self {
        Self {
            id: Uuid::new_v4(),
            path,
            info,
            thumbnails: Vec::new(),
        }
    }

    pub fn is_compatible_with(&self, other: &Clip) -> bool {
        self.info.profile == other.info.profile
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaInfo {
    pub duration: Duration,
    pub profile: CodecProfile,
}
