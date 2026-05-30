//! Custom winit `CustomApplicationHandler` that intercepts file-drop events
//! before Slint sees them (Slint itself does not surface
//! `WindowEvent::DroppedFile` to .slint code as of 1.16).

use i_slint_backend_winit::{
    winit::{event::WindowEvent, event_loop::ActiveEventLoop, window::WindowId},
    CustomApplicationHandler, EventResult,
};
use tokio::sync::mpsc;
use uuid::Uuid;
use video_merger_worker::Command;

const VIDEO_EXTS: &[&str] = &["mp4", "mov", "mkv", "avi", "webm", "m4v"];

pub struct FileDropHandler {
    pub cmd_tx: mpsc::Sender<Command>,
}

impl CustomApplicationHandler for FileDropHandler {
    fn window_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        _winit_window: Option<&i_slint_backend_winit::winit::window::Window>,
        _slint_window: Option<&slint::Window>,
        event: &WindowEvent,
    ) -> EventResult {
        if let WindowEvent::DroppedFile(path) = event {
            let ext_ok = path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| {
                    let lower = e.to_ascii_lowercase();
                    VIDEO_EXTS.iter().any(|x| *x == lower)
                })
                .unwrap_or(false);
            if ext_ok {
                let cmd = Command::Probe {
                    id: Uuid::new_v4(),
                    path: path.clone(),
                };
                if let Err(err) = self.cmd_tx.try_send(cmd) {
                    tracing::warn!(error = %err, "drop: failed to enqueue probe");
                }
            } else {
                tracing::debug!(?path, "ignored non-video dropped file");
            }
        }
        EventResult::Propagate
    }
}
