//! `cutoff` project file format (JSON).
//!
//! Embeds `Playlist` directly (which serializes each `Clip` with its
//! `MediaInfo` and trim window) so loading is one read + parse — no
//! re-probe required for the happy path. The `version` field gates
//! future migrations.

use std::path::Path;

use serde::{Deserialize, Serialize};
use video_merger_core::domain::Playlist;

use crate::PersistenceResult;

pub const CURRENT_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub version: u32,
    pub playlist: Playlist,
    #[serde(default = "default_zoom")]
    pub zoom_px_per_sec: f32,
}

fn default_zoom() -> f32 {
    6.0
}

impl Project {
    pub fn new(playlist: Playlist, zoom_px_per_sec: f32) -> Self {
        Self {
            version: CURRENT_VERSION,
            playlist,
            zoom_px_per_sec,
        }
    }

    pub fn save_to(&self, path: &Path) -> PersistenceResult<()> {
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(path, json)?;
        Ok(())
    }

    pub fn load_from(path: &Path) -> PersistenceResult<Self> {
        let bytes = std::fs::read(path)?;
        let project: Project = serde_json::from_slice(&bytes)?;
        Ok(project)
    }
}
