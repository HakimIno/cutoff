//! Snapshot-based undo/redo for the timeline.
//!
//! Each mutation pushes a clone of the *previous* playlist onto the undo
//! stack and clears the redo stack. Undo pops the top of undo onto redo and
//! restores it; redo does the inverse. Bounded at 50 snapshots — playlists
//! are small (paths + small metadata) so memory cost is negligible.

use std::sync::{Arc, Mutex};

use video_merger_core::domain::Playlist;

pub const MAX_HISTORY: usize = 50;

#[derive(Default)]
pub struct UndoStack {
    undo: Vec<Playlist>,
    redo: Vec<Playlist>,
}

pub type SharedUndo = Arc<Mutex<UndoStack>>;

impl UndoStack {
    pub fn new() -> Self {
        Self::default()
    }

    /// Push the current playlist onto the undo stack and clear redo.
    /// Call this *before* mutating.
    pub fn checkpoint(&mut self, current: &Playlist) {
        self.undo.push(current.clone());
        if self.undo.len() > MAX_HISTORY {
            // Drop the oldest snapshot to bound memory.
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    /// Move the current playlist onto the redo stack and return the prior
    /// snapshot, or `None` if there's nothing to undo.
    pub fn undo(&mut self, current: &Playlist) -> Option<Playlist> {
        let prev = self.undo.pop()?;
        self.redo.push(current.clone());
        if self.redo.len() > MAX_HISTORY {
            self.redo.remove(0);
        }
        Some(prev)
    }

    /// Inverse of [`undo`].
    pub fn redo(&mut self, current: &Playlist) -> Option<Playlist> {
        let next = self.redo.pop()?;
        self.undo.push(current.clone());
        if self.undo.len() > MAX_HISTORY {
            self.undo.remove(0);
        }
        Some(next)
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Reset both stacks (e.g. after loading a project).
    pub fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use video_merger_core::domain::{Clip, CodecProfile, MediaInfo, Resolution};

    fn clip(secs: f32) -> Clip {
        Clip::new(
            std::path::PathBuf::from(format!("/tmp/{}.mp4", secs)),
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
    fn undo_restores_prior_state() {
        let mut stack = UndoStack::new();
        let mut pl = Playlist::new();
        pl.push(clip(1.0));

        stack.checkpoint(&pl);
        pl.push(clip(2.0));
        assert_eq!(pl.len(), 2);

        let prev = stack.undo(&pl).unwrap();
        assert_eq!(prev.len(), 1);
        assert!(stack.can_redo());
    }

    #[test]
    fn redo_replays() {
        let mut stack = UndoStack::new();
        let mut pl = Playlist::new();
        pl.push(clip(1.0));
        stack.checkpoint(&pl);
        pl.push(clip(2.0));

        let undone = stack.undo(&pl).unwrap();
        let redone = stack.redo(&undone).unwrap();
        assert_eq!(redone.len(), 2);
    }

    #[test]
    fn checkpoint_clears_redo() {
        let mut stack = UndoStack::new();
        let mut pl = Playlist::new();
        pl.push(clip(1.0));
        stack.checkpoint(&pl);
        pl.push(clip(2.0));
        let _ = stack.undo(&pl);
        assert!(stack.can_redo());

        stack.checkpoint(&pl);
        assert!(!stack.can_redo(), "checkpoint should clear redo branch");
    }

    #[test]
    fn bounded_at_max_history() {
        let mut stack = UndoStack::new();
        let pl = Playlist::new();
        for _ in 0..(MAX_HISTORY + 10) {
            stack.checkpoint(&pl);
        }
        assert_eq!(stack.undo.len(), MAX_HISTORY);
    }
}
