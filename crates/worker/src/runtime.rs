use std::sync::Arc;
use std::thread;

use tokio::runtime::Builder;
use tokio::sync::mpsc;
use video_merger_engine::MergeEngine;

use crate::dispatcher::Dispatcher;
use crate::job::{Command, Event};

/// Owns the worker thread and the channels for talking to it.
///
/// Drop the handle to stop the runtime (the receiver side of `cmd_tx`
/// disconnects, the dispatcher loop exits, and the runtime is dropped).
pub struct WorkerHandle {
    pub cmd_tx: mpsc::Sender<Command>,
    pub event_rx: mpsc::UnboundedReceiver<Event>,
    _join: thread::JoinHandle<()>,
}

impl WorkerHandle {
    pub fn spawn(engine: Arc<dyn MergeEngine>) -> Self {
        let (cmd_tx, cmd_rx) = mpsc::channel::<Command>(64);
        let (event_tx, event_rx) = mpsc::unbounded_channel::<Event>();

        let join = thread::Builder::new()
            .name("video-merger-worker".into())
            .spawn(move || {
                let rt = Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .build()
                    .expect("worker tokio runtime");
                rt.block_on(async move {
                    Dispatcher::new(engine, event_tx).run(cmd_rx).await;
                });
            })
            .expect("worker thread spawn");

        Self {
            cmd_tx,
            event_rx,
            _join: join,
        }
    }
}
