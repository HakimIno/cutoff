//! FFmpeg-backed implementation of [`MergeEngine`].

pub mod cmd;
pub mod probe;
pub mod progress;
pub mod reencode;
pub mod stream_copy;

use std::path::Path;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;
use video_merger_core::domain::{ExportSpec, MediaInfo, Project};

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
        project: Project,
        spec: ExportSpec,
        progress: ProgressSink,
        cancel: CancellationToken,
    ) -> EngineResult<()> {
        let render_plan = crate::composite::build_render_plan(&project);

        let target = project
            .all_clips()
            .next()
            .map(|c| c.info.profile.clone())
            .unwrap_or_else(|| video_merger_core::domain::CodecProfile {
                video_codec: "h264".into(),
                audio_codec: "aac".into(),
                resolution: video_merger_core::domain::Resolution {
                    width: 1920,
                    height: 1080,
                },
                frame_rate_mhz: 30_000,
                pixel_format: "yuv420p".into(),
            });

        reencode::run_multitrack(
            &self.ffmpeg_bin,
            &render_plan,
            &spec,
            &target,
            spec.quality.clone(),
            progress,
            cancel,
        )
        .await
    }
}
