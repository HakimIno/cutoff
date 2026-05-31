//! Multi-track project model.
//!
//! `Project` is the top-level container that replaces the flat `Playlist` for
//! multi-track editing. Each `Track` holds a Vec of `TrackClip` — a clip
//! placed at an absolute timeline position (`start_us`).
//!
//! The old `Playlist` is still kept for serialization compatibility; use
//! `Project::from_playlist` to migrate.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::clip::{Clip, ClipId};
use crate::errors::{CoreError, CoreResult};

pub type TrackId = Uuid;

/// Top-level project model containing multiple tracks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub tracks: Vec<Track>,
}

/// Classification of a track's media type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrackKind {
    Video,
    Audio,
}

/// A single timeline track.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Track {
    pub id: TrackId,
    pub name: String,
    pub kind: TrackKind,
    pub clips: Vec<TrackClip>,
    /// Display height in logical pixels (UI hint).
    #[serde(default = "default_height")]
    pub height_px: u32,
    #[serde(default)]
    pub muted: bool,
    #[serde(default)]
    pub solo: bool,
    #[serde(default)]
    pub locked: bool,
}

fn default_height() -> u32 {
    86
}

/// A clip placed on a track at an absolute timeline position.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrackClip {
    pub clip: Clip,
    /// Absolute position on the timeline (microseconds from the start).
    pub start_us: i64,
}

// ── Project helpers ──────────────────────────────────────────────────

impl Default for Project {
    fn default() -> Self {
        Self::with_default_tracks()
    }
}

impl Project {
    /// Create a project with the standard 4 tracks: V2, V1, A1, A2.
    pub fn with_default_tracks() -> Self {
        Self {
            tracks: vec![
                Track::new("V2", TrackKind::Video),
                Track::new("V1", TrackKind::Video),
                Track::new("A1", TrackKind::Audio),
                Track::new("A2", TrackKind::Audio),
            ],
        }
    }

    /// Migrate a legacy flat `Playlist` into a `Project`.
    ///
    /// All clips are placed sequentially on the V1 track (and mirrored to
    /// A1 for audio). V2/A2 remain empty.
    pub fn from_playlist(playlist: &super::playlist::Playlist) -> Self {
        let mut project = Self::with_default_tracks();
        let v1_idx = project.track_index_by_name("V1").unwrap_or(1);

        let mut cursor_us: i64 = 0;
        for clip in playlist.clips() {
            let dur_us = clip.effective_duration_us();
            project.tracks[v1_idx].clips.push(TrackClip {
                clip: clip.clone(),
                start_us: cursor_us,
            });
            cursor_us += dur_us;
        }
        project
    }

    /// Total project duration = the latest end time across all tracks.
    pub fn duration_us(&self) -> i64 {
        self.tracks
            .iter()
            .flat_map(|t| t.clips.iter())
            .map(|tc| tc.end_us())
            .max()
            .unwrap_or(0)
    }

    /// Find a track index by name (e.g. "V1", "V2").
    pub fn track_index_by_name(&self, name: &str) -> Option<usize> {
        self.tracks.iter().position(|t| t.name == name)
    }

    /// Find the track index and clip index for a given clip ID.
    pub fn find_clip(&self, clip_id: ClipId) -> Option<(usize, usize)> {
        for (ti, track) in self.tracks.iter().enumerate() {
            for (ci, tc) in track.clips.iter().enumerate() {
                if tc.clip.id == clip_id {
                    return Some((ti, ci));
                }
            }
        }
        None
    }

    /// Flat iterator over all clips across all tracks.
    pub fn all_clips(&self) -> impl Iterator<Item = &Clip> {
        self.tracks.iter().flat_map(|t| t.clips.iter().map(|tc| &tc.clip))
    }

    /// Mutable flat iterator over all clips.
    pub fn all_clips_mut(&mut self) -> impl Iterator<Item = &mut Clip> {
        self.tracks
            .iter_mut()
            .flat_map(|t| t.clips.iter_mut().map(|tc| &mut tc.clip))
    }

    /// Total number of clips across all tracks.
    pub fn clip_count(&self) -> usize {
        self.tracks.iter().map(|t| t.clips.len()).sum()
    }

    /// Whether the project has no clips on any track.
    pub fn is_empty(&self) -> bool {
        self.tracks.iter().all(|t| t.clips.is_empty())
    }

    /// Get all video tracks, ordered by z-order (V1 first, V2 second — V2 on top).
    pub fn video_tracks(&self) -> Vec<(usize, &Track)> {
        self.tracks
            .iter()
            .enumerate()
            .filter(|(_, t)| t.kind == TrackKind::Video)
            .collect()
    }

    /// Get all audio tracks.
    pub fn audio_tracks(&self) -> Vec<(usize, &Track)> {
        self.tracks
            .iter()
            .enumerate()
            .filter(|(_, t)| t.kind == TrackKind::Audio)
            .collect()
    }

    /// Convert back to a flat playlist (V1 clips only, in start_us order).
    /// Used for backward-compatible export and legacy code paths.
    pub fn to_playlist(&self) -> super::playlist::Playlist {
        let mut pl = super::playlist::Playlist::new();
        if let Some(v1) = self.tracks.iter().find(|t| t.name == "V1") {
            let mut sorted: Vec<&TrackClip> = v1.clips.iter().collect();
            sorted.sort_by_key(|tc| tc.start_us);
            for tc in sorted {
                pl.push(tc.clip.clone());
            }
        }
        pl
    }
}

// ── Track helpers ────────────────────────────────────────────────────

impl Track {
    pub fn new(name: &str, kind: TrackKind) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: name.to_string(),
            kind,
            clips: Vec::new(),
            height_px: default_height(),
            muted: false,
            solo: false,
            locked: false,
        }
    }

    /// Duration of this track = latest clip end time.
    pub fn duration_us(&self) -> i64 {
        self.clips
            .iter()
            .map(|tc| tc.end_us())
            .max()
            .unwrap_or(0)
    }

    /// Find clip index by id.
    pub fn find_clip(&self, clip_id: ClipId) -> CoreResult<usize> {
        self.clips
            .iter()
            .position(|tc| tc.clip.id == clip_id)
            .ok_or(CoreError::ClipNotFound(clip_id))
    }

    /// Remove a clip by id and return it.
    pub fn remove_clip(&mut self, clip_id: ClipId) -> CoreResult<TrackClip> {
        let idx = self.find_clip(clip_id)?;
        Ok(self.clips.remove(idx))
    }

    /// Check if inserting a clip at [start_us, start_us + dur_us) would
    /// overlap any existing clip (excluding `exclude_id` if provided).
    pub fn would_overlap(
        &self,
        start_us: i64,
        dur_us: i64,
        exclude_id: Option<ClipId>,
    ) -> bool {
        let end_us = start_us + dur_us;
        self.clips.iter().any(|tc| {
            if exclude_id == Some(tc.clip.id) {
                return false;
            }
            let tc_end = tc.end_us();
            // Two intervals overlap iff start < other_end && other_start < end
            start_us < tc_end && tc.start_us < end_us
        })
    }
}

// ── TrackClip helpers ────────────────────────────────────────────────

impl TrackClip {
    /// End time of this clip on the timeline.
    pub fn end_us(&self) -> i64 {
        self.start_us + self.clip.effective_duration_us()
    }
}

// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{CodecProfile, MediaInfo, Resolution};
    use std::path::PathBuf;
    use std::time::Duration;

    fn make_clip(secs: f32) -> Clip {
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
    fn default_project_has_four_tracks() {
        let proj = Project::with_default_tracks();
        assert_eq!(proj.tracks.len(), 4);
        assert_eq!(proj.tracks[0].name, "V2");
        assert_eq!(proj.tracks[1].name, "V1");
        assert_eq!(proj.tracks[2].name, "A1");
        assert_eq!(proj.tracks[3].name, "A2");
    }

    #[test]
    fn from_playlist_places_clips_sequentially() {
        let mut pl = crate::domain::playlist::Playlist::new();
        pl.push(make_clip(5.0));
        pl.push(make_clip(3.0));
        let proj = Project::from_playlist(&pl);

        let v1 = &proj.tracks[1]; // V1
        assert_eq!(v1.clips.len(), 2);
        assert_eq!(v1.clips[0].start_us, 0);
        assert_eq!(v1.clips[1].start_us, 5_000_000);
    }

    #[test]
    fn duration_us_is_max_end() {
        let mut proj = Project::with_default_tracks();
        let v1_idx = proj.track_index_by_name("V1").unwrap();
        proj.tracks[v1_idx].clips.push(TrackClip {
            clip: make_clip(10.0),
            start_us: 0,
        });
        // V2 clip starts at 3s, 5s long → ends at 8s. V1 ends at 10s.
        let v2_idx = proj.track_index_by_name("V2").unwrap();
        proj.tracks[v2_idx].clips.push(TrackClip {
            clip: make_clip(5.0),
            start_us: 3_000_000,
        });
        assert_eq!(proj.duration_us(), 10_000_000);
    }

    #[test]
    fn overlap_detection() {
        let mut track = Track::new("V1", TrackKind::Video);
        let clip = make_clip(5.0);
        track.clips.push(TrackClip {
            clip,
            start_us: 2_000_000,
        });
        // [2s, 7s) existing. Try to place [4s, 6s) → overlap
        assert!(track.would_overlap(4_000_000, 2_000_000, None));
        // [0s, 2s) → no overlap (edge)
        assert!(!track.would_overlap(0, 2_000_000, None));
        // [7s, 10s) → no overlap (edge)
        assert!(!track.would_overlap(7_000_000, 3_000_000, None));
    }

    #[test]
    fn find_clip_across_tracks() {
        let mut proj = Project::with_default_tracks();
        let clip = make_clip(5.0);
        let clip_id = clip.id;
        proj.tracks[0].clips.push(TrackClip { clip, start_us: 0 });
        let (ti, ci) = proj.find_clip(clip_id).unwrap();
        assert_eq!(ti, 0);
        assert_eq!(ci, 0);
    }
}
