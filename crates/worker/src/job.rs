use std::path::PathBuf;

use uuid::Uuid;
use video_merger_core::domain::{Clip, ClipId, ExportSpec, MediaInfo, Project};
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
        project: Project,
        spec: ExportSpec,
    },
    Cancel {
        id: JobId,
    },
    // --- Preview / thumbnails (Phase 1) ---
    /// Open `path` as the active preview source. Replaces any prior session.
    /// `trim_in_us` / `trim_out_us` bound playback and seeking; pass 0 / source_duration
    /// when the clip is untrimmed.
    OpenPreview {
        project: Project,
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
    /// Extract waveform peaks for `clip_id` → write binary cache file.
    GenerateWaveform {
        clip_id: ClipId,
        path: PathBuf,
        out_path: PathBuf,
    },
    /// Toggle master mute on the live audio output.
    SetMasterMuted(bool),
    /// Adjust master volume (0.0..=2.0).
    SetMasterVolume(f32),
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
    AudioMeter {
        peak_l: f32,
        peak_r: f32,
    },
    ThumbnailsReady {
        clip_id: ClipId,
        paths: Vec<PathBuf>,
    },
    WaveformReady {
        clip_id: ClipId,
        path: PathBuf,
    },
}
