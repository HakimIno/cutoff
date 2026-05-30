use std::path::PathBuf;

use uuid::Uuid;
use video_merger_core::domain::{Clip, ClipId, MediaInfo};
use video_merger_core::services::MergePlan;
use video_merger_engine::DecodedFrame;

pub type JobId = Uuid;

/// Commands sent from the UI thread into the worker.
#[derive(Debug)]
pub enum Command {
    Probe {
        id: JobId,
        path: PathBuf,
    },
    Merge {
        id: JobId,
        plan: MergePlan,
    },
    Cancel {
        id: JobId,
    },
    // --- Preview / thumbnails (Phase 1) ---
    /// Open `path` as the active preview source. Replaces any prior session.
    OpenPreview {
        clip_id: ClipId,
        path: PathBuf,
    },
    /// Start playing the active preview at native frame rate.
    PlayPreview,
    /// Pause the active preview (decoder retains position).
    PausePreview,
    /// Seek the active preview to `pts_us` microseconds.
    SeekPreview {
        pts_us: i64,
    },
    /// Generate `count` evenly-spaced filmstrip thumbnails for `clip_id`.
    GenerateThumbnails {
        clip_id: ClipId,
        path: PathBuf,
        out_dir: PathBuf,
        count: usize,
    },
    Shutdown,
}

/// Events sent from the worker back to the UI thread.
#[derive(Debug, Clone)]
pub enum Event {
    Probed {
        id: JobId,
        path: PathBuf,
        info: MediaInfo,
    },
    ClipReady(Clip),
    Progress {
        id: JobId,
        fraction: f32,
        processed_secs: f64,
    },
    Finished {
        id: JobId,
        output: PathBuf,
    },
    Failed {
        id: JobId,
        message: String,
    },
    Cancelled {
        id: JobId,
    },
    // --- Preview / thumbnails (Phase 1) ---
    PreviewOpened {
        clip_id: ClipId,
        duration_us: i64,
        width: u32,
        height: u32,
        frame_rate_num: i32,
        frame_rate_den: i32,
    },
    FrameReady {
        clip_id: ClipId,
        frame: DecodedFrame,
    },
    PreviewEnded {
        clip_id: ClipId,
    },
    ThumbnailsReady {
        clip_id: ClipId,
        paths: Vec<PathBuf>,
    },
}
