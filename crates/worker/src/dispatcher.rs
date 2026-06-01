use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::{mpsc, Mutex};
use tokio_util::sync::CancellationToken;
use video_merger_engine::audio::waveform as wf;
use video_merger_engine::{decoder, EngineError, MergeEngine, ProgressEvent};

use crate::job::{Command, Event, JobId};
use crate::preview::{spawn_preview, PreviewCtrl, PreviewHandle};
use crate::progress;

pub struct Dispatcher {
    engine: Arc<dyn MergeEngine>,
    event_tx: mpsc::UnboundedSender<Event>,
    cancels: Arc<Mutex<HashMap<JobId, CancellationToken>>>,
    preview: Option<PreviewHandle>,
}

impl Dispatcher {
    pub fn new(engine: Arc<dyn MergeEngine>, event_tx: mpsc::UnboundedSender<Event>) -> Self {
        Self {
            engine,
            event_tx,
            cancels: Arc::new(Mutex::new(HashMap::new())),
            preview: None,
        }
    }

    pub async fn run(mut self, mut cmd_rx: mpsc::Receiver<Command>) {
        while let Some(cmd) = cmd_rx.recv().await {
            match cmd {
                Command::Probe { id, path } => {
                    let engine = self.engine.clone();
                    let event_tx = self.event_tx.clone();
                    tokio::spawn(async move {
                        match engine.probe(&path).await {
                            Ok(info) => {
                                let _ = event_tx.send(Event::Probed { id, path, info });
                            }
                            Err(e) => {
                                let _ = event_tx.send(Event::Failed {
                                    id,
                                    message: e.to_string(),
                                });
                            }
                        }
                    });
                }
                Command::Merge { id, project, spec } => {
                    let engine = self.engine.clone();
                    let event_tx = self.event_tx.clone();
                    let token = CancellationToken::new();
                    self.cancels.lock().await.insert(id, token.clone());
                    let cancels = self.cancels.clone();

                    let (prog_tx, prog_rx) = mpsc::unbounded_channel::<ProgressEvent>();
                    progress::forward(id, prog_rx, event_tx.clone());

                    let output = spec.output_path.clone();
                    tokio::spawn(async move {
                        let result = engine.execute(project, spec, prog_tx, token).await;
                        cancels.lock().await.remove(&id);
                        let event = match result {
                            Ok(_) => Event::Finished { id, output },
                            Err(EngineError::Cancelled) => Event::Cancelled { id },
                            Err(e) => Event::Failed {
                                id,
                                message: e.to_string(),
                            },
                        };
                        let _ = event_tx.send(event);
                    });
                }
                Command::Cancel { id } => {
                    if let Some(token) = self.cancels.lock().await.get(&id) {
                        token.cancel();
                    }
                }
                Command::OpenPreview { project } => {
                    if let Some(h) = &self.preview {
                        h.send(PreviewCtrl::UpdateProject(project));
                    } else {
                        self.preview = Some(spawn_preview(project, self.event_tx.clone()));
                    }
                }
                Command::PlayPreview => {
                    if let Some(h) = &self.preview {
                        h.send(PreviewCtrl::Play);
                    }
                }
                Command::PausePreview => {
                    if let Some(h) = &self.preview {
                        h.send(PreviewCtrl::Pause);
                    }
                }
                Command::SeekPreview { pts_us } => {
                    if let Some(h) = &self.preview {
                        h.send(PreviewCtrl::Seek { pts_us });
                    }
                }
                Command::GenerateThumbnails {
                    clip_id,
                    path,
                    out_dir,
                    count,
                } => {
                    let event_tx = self.event_tx.clone();
                    tokio::task::spawn_blocking(move || {
                        match decoder::extract_thumbnails(&path, &out_dir, count, 160, 90) {
                            Ok(paths) => {
                                let _ = event_tx.send(Event::ThumbnailsReady { clip_id, paths });
                            }
                            Err(e) => {
                                tracing::warn!(error = %e, ?path, "thumbnail extraction failed");
                            }
                        }
                    });
                }
                Command::GenerateWaveform {
                    clip_id,
                    path,
                    out_path,
                } => {
                    let event_tx = self.event_tx.clone();
                    tokio::task::spawn_blocking(move || {
                        if let Some(parent) = out_path.parent() {
                            let _ = std::fs::create_dir_all(parent);
                        }
                        match wf::extract_waveform(&path) {
                            Ok(Some(peaks)) => {
                                if let Err(e) = peaks.save_to(&out_path) {
                                    tracing::warn!(error = %e, "save waveform failed");
                                    return;
                                }
                                let _ = event_tx.send(Event::WaveformReady {
                                    clip_id,
                                    path: out_path,
                                });
                            }
                            Ok(None) => {
                                tracing::debug!(?path, "no audio stream — skip waveform");
                            }
                            Err(e) => {
                                tracing::warn!(error = %e, ?path, "waveform extraction failed");
                            }
                        }
                    });
                }
                Command::SetMasterMuted(m) => {
                    if let Some(h) = &self.preview {
                        h.send(PreviewCtrl::SetMuted(m));
                    }
                }
                Command::SetMasterVolume(v) => {
                    if let Some(h) = &self.preview {
                        h.send(PreviewCtrl::SetVolume(v));
                    }
                }
                Command::Shutdown => break,
            }
        }
    }
}
