//! Glue layer between the Slint UI and the worker thread.

mod callbacks;
mod events;
pub mod models;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use uuid::Uuid;
use video_merger_core::domain::Playlist;
use video_merger_persistence::{AppConfig, History, Storage};
use video_merger_ui::AppWindow;
use video_merger_worker::{JobId, WorkerHandle};

pub type SharedPlaylist = Arc<Mutex<Playlist>>;
pub type ActiveJob = Arc<Mutex<Option<JobId>>>;
pub type JobMetaMap = Arc<Mutex<HashMap<JobId, JobMeta>>>;
pub type SharedConfig = Arc<Mutex<AppConfig>>;
pub type SharedHistory = Arc<Mutex<History>>;
pub type SharedPreview = Arc<Mutex<PreviewState>>;

/// Metadata captured at export-submission time, looked up when the
/// terminal event for that job arrives.
#[derive(Debug, Clone)]
pub struct JobMeta {
    pub output_path: PathBuf,
    pub input_count: usize,
    pub duration: Duration,
}

/// Live preview session state shared across callbacks and events.
#[derive(Debug, Default, Clone)]
pub struct PreviewState {
    pub clip_id: Option<Uuid>,
    pub duration_us: i64,
    pub playhead_us: i64,
    pub playing: bool,
}

#[derive(Clone)]
pub struct BridgeState {
    pub playlist: SharedPlaylist,
    pub active_job: ActiveJob,
    pub job_meta: JobMetaMap,
    pub config: SharedConfig,
    pub history: SharedHistory,
    pub storage: Arc<Storage>,
    pub preview: SharedPreview,
}

pub fn install(window: &AppWindow, worker: WorkerHandle, state: BridgeState) {
    let WorkerHandle {
        cmd_tx, event_rx, ..
    } = worker;
    callbacks::install(window, cmd_tx.clone(), state.clone());
    events::install(window, event_rx, cmd_tx, state);
}
