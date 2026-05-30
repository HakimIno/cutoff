//! FFmpeg-backed implementation of [`MergeEngine`].

pub mod cmd;
pub mod probe;
pub mod progress;
pub mod reencode;
pub mod stream_copy;

use std::path::Path;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;
use video_merger_core::domain::MediaInfo;
use video_merger_core::services::{MergePlan, MergeStrategy};

use crate::traits::{MergeEngine, ProgressSink};
use crate::EngineResult;

pub struct FfmpegEngine {
    pub ffmpeg_bin: String,
    pub ffprobe_bin: String,
}

impl Default for FfmpegEngine {
    fn default() -> Self {
        Self {
            ffmpeg_bin: "ffmpeg".into(),
            ffprobe_bin: "ffprobe".into(),
        }
    }
}

#[async_trait]
impl MergeEngine for FfmpegEngine {
    async fn probe(&self, path: &Path) -> EngineResult<MediaInfo> {
        probe::probe(&self.ffprobe_bin, path).await
    }

    async fn execute(
        &self,
        plan: MergePlan,
        progress: ProgressSink,
        cancel: CancellationToken,
    ) -> EngineResult<()> {
        match &plan.strategy {
            MergeStrategy::StreamCopy => {
                stream_copy::run(&self.ffmpeg_bin, &plan, progress, cancel).await
            }
            MergeStrategy::Reencode { .. } => {
                reencode::run(&self.ffmpeg_bin, &plan, progress, cancel).await
            }
        }
    }
}
