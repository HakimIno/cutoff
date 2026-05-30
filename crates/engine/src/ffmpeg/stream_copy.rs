use std::path::{Path, PathBuf};

use tokio_util::sync::CancellationToken;
use video_merger_core::services::MergePlan;

use crate::ffmpeg::{cmd, progress};
use crate::traits::ProgressSink;
use crate::{EngineError, EngineResult};

/// Lossless merge path: builds a concat-demuxer file list and runs
/// `ffmpeg -f concat -safe 0 -i list.txt -c copy <out>`.
pub async fn run(
    bin: &str,
    plan: &MergePlan,
    progress_tx: ProgressSink,
    cancel: CancellationToken,
) -> EngineResult<()> {
    let list_path = write_concat_list(&plan.inputs).await?;

    let result = run_ffmpeg(bin, plan, &list_path, progress_tx, cancel).await;

    // Clean up the temp concat file regardless of outcome.
    let _ = tokio::fs::remove_file(&list_path).await;

    result
}

async fn write_concat_list(inputs: &[video_merger_core::services::MergeInput]) -> EngineResult<PathBuf> {
    let path = std::env::temp_dir().join(format!("video-merger-concat-{}.txt", uuid::Uuid::new_v4()));
    let mut contents = String::new();
    for input in inputs {
        // The concat demuxer treats single quotes as delimiters; escape them.
        let escaped = input.path.to_string_lossy().replace('\'', r"'\''");
        contents.push_str("file '");
        contents.push_str(&escaped);
        contents.push_str("'\n");
        // StreamCopy is only chosen when no clip is trimmed, so we don't emit
        // inpoint/outpoint directives here (they're keyframe-snapped and
        // unreliable for lossless export).
    }
    tokio::fs::write(&path, contents).await?;
    Ok(path)
}

async fn run_ffmpeg(
    bin: &str,
    plan: &MergePlan,
    list_path: &Path,
    progress_tx: ProgressSink,
    cancel: CancellationToken,
) -> EngineResult<()> {
    let output = plan.spec.output_path.to_string_lossy().into_owned();
    let args: Vec<String> = vec![
        "-y".into(),
        "-hide_banner".into(),
        "-loglevel".into(),
        "error".into(),
        "-f".into(),
        "concat".into(),
        "-safe".into(),
        "0".into(),
        "-i".into(),
        list_path.to_string_lossy().into_owned(),
        "-c".into(),
        "copy".into(),
        "-progress".into(),
        "pipe:1".into(),
        "-nostats".into(),
        output,
    ];

    tracing::info!(?args, "spawning ffmpeg stream-copy");
    let mut child = cmd::spawn(bin, &args)?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| EngineError::ProbeParse("ffmpeg stdout pipe missing".into()))?;

    let total_secs = plan.total_duration.as_secs_f64();
    let pump_handle = tokio::spawn(progress::pump(stdout, total_secs, progress_tx));

    let outcome: EngineResult<()> = tokio::select! {
        status = child.wait() => {
            let status = status?;
            if status.success() {
                Ok(())
            } else {
                Err(EngineError::NonZeroExit(status.code().unwrap_or(-1)))
            }
        }
        _ = cancel.cancelled() => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            Err(EngineError::Cancelled)
        }
    };

    let _ = pump_handle.await;
    outcome
}
