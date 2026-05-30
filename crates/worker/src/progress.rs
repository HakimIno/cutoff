use tokio::sync::mpsc;
use video_merger_engine::ProgressEvent;

use crate::job::{Event, JobId};

/// Bridge engine `ProgressEvent`s into the UI-facing `Event::Progress` stream.
pub fn forward(
    job_id: JobId,
    mut engine_rx: mpsc::UnboundedReceiver<ProgressEvent>,
    event_tx: mpsc::UnboundedSender<Event>,
) {
    tokio::spawn(async move {
        while let Some(ev) = engine_rx.recv().await {
            let _ = event_tx.send(Event::Progress {
                id: job_id,
                fraction: ev.fraction,
                processed_secs: ev.processed_secs,
            });
        }
    });
}
