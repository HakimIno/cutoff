//! Glue layer between the Slint UI and the worker thread.

mod callbacks;
mod events;
pub mod models;
pub mod timeline_view;
pub mod undo;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use uuid::Uuid;
use video_merger_core::domain::{Playlist, Project};
use video_merger_persistence::{AppConfig, History, Storage};
use video_merger_ui::AppWindow;
use video_merger_worker::{JobId, WorkerHandle};

/// Shared project: the new multi-track project model.
/// Replaces the old `SharedPlaylist` as the source of truth.
pub type SharedProject = Arc<Mutex<Project>>;
/// Legacy alias — wraps a Playlist for backward-compat callers.
/// DEPRECATED: use SharedProject.
pub type SharedPlaylist = Arc<Mutex<Playlist>>;
pub type ActiveJob = Arc<Mutex<Option<JobId>>>;
pub type JobMetaMap = Arc<Mutex<HashMap<JobId, JobMeta>>>;
pub type SharedConfig = Arc<Mutex<AppConfig>>;
pub type SharedHistory = Arc<Mutex<History>>;
pub type SharedPreview = Arc<Mutex<PreviewState>>;
pub type SharedZoom = Arc<Mutex<f32>>;
pub type SharedAudio = Arc<Mutex<AudioState>>;

pub use undo::SharedUndo;

#[derive(Debug, Default, Clone)]
pub struct AudioState {
    pub master_muted: bool,
    #[allow(dead_code)]
    pub master_volume: f32, // 1.0 = unity
}

/// Per-track UI/playback flags. Indexed by `project.tracks` index.
#[derive(Debug, Default, Clone)]
pub struct TracksState {
    pub muted: Vec<bool>,
    pub soloed: Vec<bool>,
    pub locked: Vec<bool>,
}

impl TracksState {
    /// Initialize flags for a project's current track count, all `false`.
    pub fn for_project(project: &video_merger_core::domain::Project) -> Self {
        let n = project.tracks.len();
        Self {
            muted: vec![false; n],
            soloed: vec![false; n],
            locked: vec![false; n],
        }
    }

    /// Resize all three vectors to match `n`, preserving existing values
    /// and defaulting new slots to `false`.
    pub fn resize(&mut self, n: usize) {
        self.muted.resize(n, false);
        self.soloed.resize(n, false);
        self.locked.resize(n, false);
    }

    /// Remove the flag slot at `idx`, shifting later entries down.
    pub fn remove(&mut self, idx: usize) {
        if idx < self.muted.len() { self.muted.remove(idx); }
        if idx < self.soloed.len() { self.soloed.remove(idx); }
        if idx < self.locked.len() { self.locked.remove(idx); }
    }
}

pub type SharedTracks = Arc<Mutex<TracksState>>;

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
    /// When set, applied as a `SeekPreview` on the next `PreviewOpened` event.
    /// Lets a single seek-fraction click both switch clips and seek inside the new one.
    pub pending_seek_us: Option<i64>,
    /// Rolling FrameReady arrival timestamps for FPS calculation. Trimmed to
    /// the last second on each push.
    pub frame_history: std::collections::VecDeque<std::time::Instant>,
}

#[derive(Clone)]
pub struct BridgeState {
    /// Legacy playlist — kept in sync with `project.tracks[v1].clips` for
    /// backward compat with existing callbacks that haven't been migrated yet.
    pub playlist: SharedPlaylist,
    /// Multi-track project model (source of truth for Phase 8+).
    pub project: SharedProject,
    pub active_job: ActiveJob,
    pub job_meta: JobMetaMap,
    pub config: SharedConfig,
    pub history: SharedHistory,
    pub storage: Arc<Storage>,
    pub preview: SharedPreview,
    pub zoom: SharedZoom,
    pub undo: SharedUndo,
    pub audio: SharedAudio,
    pub tracks: SharedTracks,
}

pub const ZOOM_MIN: f32 = 1.0;
pub const ZOOM_MAX: f32 = 200.0;
pub const ZOOM_DEFAULT: f32 = 6.0;

pub fn install(window: &AppWindow, worker: WorkerHandle, state: BridgeState) {
    let WorkerHandle {
        cmd_tx, event_rx, ..
    } = worker;
    callbacks::install(window, cmd_tx.clone(), state.clone());
    events::install(window, event_rx, cmd_tx, state);
}

/// Synchronize the legacy Playlist from the Project's V1 track.
/// Call this after mutating the Project to keep backward-compat callers happy.
#[allow(dead_code)]
pub fn sync_playlist_from_project(state: &BridgeState) {
    let playlist = {
        let proj = state.project.lock().expect("project mutex poisoned");
        proj.to_playlist()
    };
    let mut pl = state.playlist.lock().expect("playlist mutex poisoned");
    *pl = playlist;
}
