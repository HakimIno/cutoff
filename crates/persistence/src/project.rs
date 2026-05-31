//! `cutoff` project file format (JSON).
//!
//! **Version history:**
//! - v1: initial flat playlist
//! - v2: added `zoom_px_per_sec`, clip `video_track` field
//! - v3: multi-track architecture — `Project` wraps `Vec<Track>` instead
//!        of a flat `Playlist`. On load, v1/v2 files are migrated by
//!        wrapping the playlist into a single V1 video track.

use std::path::Path;

use serde::{Deserialize, Serialize};
use video_merger_core::domain::{Playlist, Project as CoreProject};

use crate::PersistenceResult;

pub const CURRENT_VERSION: u32 = 3;

/// On-disk format v3 — multi-track.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub version: u32,
    /// Multi-track project data (v3+).
    #[serde(default)]
    pub project: Option<CoreProject>,
    /// Legacy playlist (v1/v2 only, `None` for v3+).
    #[serde(default)]
    pub playlist: Option<Playlist>,
    #[serde(default = "default_zoom")]
    pub zoom_px_per_sec: f32,
}

fn default_zoom() -> f32 {
    6.0
}

impl Project {
    /// Create a v3 project from a `CoreProject`.
    pub fn new(core_project: CoreProject, zoom_px_per_sec: f32) -> Self {
        Self {
            version: CURRENT_VERSION,
            project: Some(core_project),
            playlist: None,
            zoom_px_per_sec,
        }
    }

    /// Create a v3 project from a legacy Playlist (for backward compat).
    pub fn from_playlist(playlist: Playlist, zoom_px_per_sec: f32) -> Self {
        let core_project = CoreProject::from_playlist(&playlist);
        Self {
            version: CURRENT_VERSION,
            project: Some(core_project),
            playlist: None,
            zoom_px_per_sec,
        }
    }

    /// Get the `CoreProject`, migrating if necessary.
    pub fn core_project(&self) -> CoreProject {
        if let Some(ref proj) = self.project {
            proj.clone()
        } else if let Some(ref playlist) = self.playlist {
            // v1/v2 migration: wrap legacy playlist into a single V1 track.
            CoreProject::from_playlist(playlist)
        } else {
            CoreProject::with_default_tracks()
        }
    }

    /// Get a playlist-compatible view (for legacy code paths).
    pub fn playlist_compat(&self) -> Playlist {
        self.core_project().to_playlist()
    }

    pub fn save_to(&self, path: &Path) -> PersistenceResult<()> {
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(path, json)?;
        Ok(())
    }

    pub fn load_from(path: &Path) -> PersistenceResult<Self> {
        let bytes = std::fs::read(path)?;
        let mut project: Project = serde_json::from_slice(&bytes)?;
        // Auto-migrate old versions to v3.
        if project.version < 3 {
            project = migrate_to_v3(project);
        }
        Ok(project)
    }
}

/// Migrate a v1/v2 project to v3.
fn migrate_to_v3(old: Project) -> Project {
    let playlist = old.playlist.unwrap_or_default();
    let core_project = CoreProject::from_playlist(&playlist);
    Project {
        version: CURRENT_VERSION,
        project: Some(core_project),
        playlist: None,
        zoom_px_per_sec: old.zoom_px_per_sec,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migration_v2_to_v3() {
        let old = Project {
            version: 2,
            project: None,
            playlist: Some(Playlist::new()),
            zoom_px_per_sec: 6.0,
        };
        let migrated = migrate_to_v3(old);
        assert_eq!(migrated.version, 3);
        assert!(migrated.project.is_some());
        assert!(migrated.playlist.is_none());
    }

    #[test]
    fn core_project_from_empty() {
        let proj = Project {
            version: 3,
            project: None,
            playlist: None,
            zoom_px_per_sec: 6.0,
        };
        let core = proj.core_project();
        assert_eq!(core.tracks.len(), 4);
    }
}
