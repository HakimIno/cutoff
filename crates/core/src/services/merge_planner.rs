use std::path::PathBuf;
use std::time::Duration;

use super::compatibility::{CompatibilityAnalyzer, MergeStrategy};
use crate::domain::{ExportSpec, Playlist};
use crate::errors::{CoreError, CoreResult};

/// One concat-source for the merge pipeline. Carries trim info so the
/// engine can emit per-input `-ss`/`-to` (or concat-demuxer in/outpoints).
#[derive(Debug, Clone)]
pub struct MergeInput {
    pub path: PathBuf,
    pub trim_in_us: i64,
    pub trim_out_us: i64,
}

impl MergeInput {
    pub fn is_trimmed(&self, source_duration_us: i64) -> bool {
        self.trim_in_us > 0 || self.trim_out_us < source_duration_us
    }
}

/// An immutable, engine-agnostic description of a merge job.
#[derive(Debug, Clone)]
pub struct MergePlan {
    pub inputs: Vec<MergeInput>,
    pub spec: ExportSpec,
    pub strategy: MergeStrategy,
    /// Sum of *effective* (post-trim) clip durations. Engines use this to
    /// normalize progress into 0..1.
    pub total_duration: Duration,
}

pub struct MergePlanner;

impl MergePlanner {
    pub fn build(playlist: &Playlist, spec: ExportSpec) -> CoreResult<MergePlan> {
        if playlist.is_empty() {
            return Err(CoreError::EmptyPlaylist);
        }
        let any_trimmed = playlist.clips().iter().any(|c| c.is_trimmed());
        let strategy = CompatibilityAnalyzer::analyze(playlist)
            .ok_or(CoreError::EmptyPlaylist)?;
        // Stream-copy with mid-stream trim is keyframe-snapped (visible
        // glitches). Force a re-encode whenever any clip is trimmed.
        let strategy = if any_trimmed {
            match strategy {
                MergeStrategy::StreamCopy => MergeStrategy::Reencode {
                    target: playlist.clips()[0].info.profile.clone(),
                },
                other => other,
            }
        } else {
            strategy
        };

        let inputs = playlist
            .clips()
            .iter()
            .map(|c| MergeInput {
                path: c.path.clone(),
                trim_in_us: c.trim_in_us(),
                trim_out_us: c.trim_out_us(),
            })
            .collect();
        let total_duration = playlist
            .clips()
            .iter()
            .map(|c| c.effective_duration())
            .sum();
        Ok(MergePlan {
            inputs,
            spec,
            strategy,
            total_duration,
        })
    }
}
