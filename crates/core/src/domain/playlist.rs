use serde::{Deserialize, Serialize};

use super::clip::{Clip, ClipId};
use crate::errors::{CoreError, CoreResult};

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Playlist {
    clips: Vec<Clip>,
}

impl Playlist {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, clip: Clip) {
        self.clips.push(clip);
    }

    pub fn remove(&mut self, id: ClipId) -> CoreResult<Clip> {
        let pos = self
            .clips
            .iter()
            .position(|c| c.id == id)
            .ok_or(CoreError::ClipNotFound(id))?;
        Ok(self.clips.remove(pos))
    }

    pub fn reorder(&mut self, from: usize, to: usize) -> CoreResult<()> {
        let len = self.clips.len();
        if from >= len {
            return Err(CoreError::InvalidReorder { index: from, len });
        }
        if to >= len {
            return Err(CoreError::InvalidReorder { index: to, len });
        }
        let clip = self.clips.remove(from);
        self.clips.insert(to, clip);
        Ok(())
    }

    pub fn clips(&self) -> &[Clip] {
        &self.clips
    }

    pub fn clips_mut(&mut self) -> &mut [Clip] {
        &mut self.clips
    }

    pub fn insert(&mut self, index: usize, clip: Clip) -> CoreResult<()> {
        let len = self.clips.len();
        if index > len {
            return Err(CoreError::InvalidReorder { index, len });
        }
        self.clips.insert(index, clip);
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.clips.len()
    }

    pub fn is_empty(&self) -> bool {
        self.clips.is_empty()
    }
}
