use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use video_merger_core::domain::{ExportSpec, MediaInfo, Project};

use crate::EngineResult;

/// Cross-thread progress event emitted while a merge job runs.
#[derive(Debug, Clone)]
pub struct ProgressEvent {
    pub fraction: f32,
    pub processed_secs: f64,
    pub eta_secs: Option<f64>,
}

/// Sink that the engine emits progress events into.
pub type ProgressSink = mpsc::UnboundedSender<ProgressEvent>;

/// The swap-point. Replace the FFmpeg implementation with GStreamer (or a
/// mock for tests) by changing the type bound at the call site.
#[async_trait]
pub trait MergeEngine: Send + Sync + 'static {
    async fn probe(&self, path: &Path) -> EngineResult<MediaInfo>;

    async fn execute(
        &self,
        project: Project,
        spec: ExportSpec,
        progress: ProgressSink,
        cancel: CancellationToken,
    ) -> EngineResult<()>;
}

pub type SharedEngine = Arc<dyn MergeEngine>;
