//! Composition root. Wires the UI, worker, and engine together.

mod bridge;
mod view_model;

use std::collections::HashMap;
use std::path::Path;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use slint::{ComponentHandle, ModelRc, VecModel};
use tracing_subscriber::EnvFilter;
use video_merger_core::domain::Playlist;
use video_merger_engine::ffmpeg::FfmpegEngine;
use video_merger_persistence::{AppConfig, History, Storage};
use video_merger_ui::{AppWindow, ClipData};
use video_merger_worker::WorkerHandle;

use bridge::BridgeState;

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    // Load persisted state. Each `.unwrap_or_default()` keeps the app
    // launchable on a fresh install where these files do not yet exist.
    let storage = Arc::new(Storage::for_app()?);
    let config = AppConfig::load(&storage).unwrap_or_else(|e| {
        tracing::warn!(error = %e, "config load failed, using defaults");
        AppConfig::default()
    });
    let history = History::load(&storage).unwrap_or_else(|e| {
        tracing::warn!(error = %e, "history load failed, starting empty");
        History::default()
    });
    tracing::info!(records = history.records.len(), "history loaded");

    let ffmpeg_bin = config
        .ffmpeg_path
        .clone()
        .unwrap_or_else(|| "ffmpeg".into());
    let ffprobe_bin = derive_ffprobe(&ffmpeg_bin);

    let engine = Arc::new(FfmpegEngine {
        ffmpeg_bin,
        ffprobe_bin,
    });
    let worker = WorkerHandle::spawn(engine);

    let window = AppWindow::new()?;

    let clips_model: Rc<VecModel<ClipData>> = Rc::new(VecModel::default());
    window.set_clips(ModelRc::from(clips_model));
    window.set_status_text("Ready".into());
    window.set_exporting(false);

    // Shared state. Arc<Mutex<>> because the event-bridge thread marshals
    // mutations into the UI thread via Send-bounded closures.
    let state = BridgeState {
        playlist: Arc::new(Mutex::new(Playlist::new())),
        active_job: Arc::new(Mutex::new(None)),
        job_meta: Arc::new(Mutex::new(HashMap::new())),
        config: Arc::new(Mutex::new(config)),
        history: Arc::new(Mutex::new(history)),
        storage,
        preview: Arc::new(Mutex::new(Default::default())),
    };

    bridge::install(&window, worker, state);

    window.run()?;
    Ok(())
}

/// If `ffmpeg_path` is an absolute path, look for `ffprobe` next to it.
/// Otherwise fall back to plain `ffprobe` on PATH.
fn derive_ffprobe(ffmpeg_path: &str) -> String {
    let p = Path::new(ffmpeg_path);
    if let Some(parent) = p.parent() {
        if !parent.as_os_str().is_empty() {
            return parent.join("ffprobe").to_string_lossy().into_owned();
        }
    }
    "ffprobe".into()
}
