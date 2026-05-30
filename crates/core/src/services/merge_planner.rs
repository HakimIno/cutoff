use std::path::PathBuf;
use std::time::Duration;

use super::compatibility::{CompatibilityAnalyzer, MergeStrategy};
use crate::domain::{ExportSpec, Playlist};
use crate::errors::{CoreError, CoreResult};

/// An immutable, engine-agnostic description of a merge job.
#[derive(Debug, Clone)]
pub struct MergePlan {
    pub inputs: Vec<PathBuf>,
    pub spec: ExportSpec,
    pub strategy: MergeStrategy,
    /// Sum of clip durations. Engines use this to normalize progress into 0..1.
    pub total_duration: Duration,
}

pub struct MergePlanner;

impl MergePlanner {
    pub fn build(playlist: &Playlist, spec: ExportSpec) -> CoreResult<MergePlan> {
        if playlist.is_empty() {
            return Err(CoreError::EmptyPlaylist);
        }
        let strategy = CompatibilityAnalyzer::analyze(playlist)
            .ok_or(CoreError::EmptyPlaylist)?;
        let inputs = playlist
            .clips()
            .iter()
            .map(|c| c.path.clone())
            .collect();
        let total_duration = playlist
            .clips()
            .iter()
            .map(|c| c.info.duration)
            .sum();
        Ok(MergePlan {
            inputs,
            spec,
            strategy,
            total_duration,
        })
    }
}
