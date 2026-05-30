use std::fmt::Write as _;

use tokio_util::sync::CancellationToken;
use video_merger_core::domain::{CodecProfile, Quality};
use video_merger_core::services::{MergePlan, MergeStrategy};

use crate::ffmpeg::{cmd, progress};
use crate::traits::ProgressSink;
use crate::{EngineError, EngineResult};

/// Re-encoding merge path. Used when clips have mismatched codec profiles.
///
/// Each input is normalized (scaled+padded, fps, pixel format, audio format)
/// into the target profile then joined with the `concat` filter and
/// re-encoded with libx264 / aac.
pub async fn run(
    bin: &str,
    plan: &MergePlan,
    progress_tx: ProgressSink,
    cancel: CancellationToken,
) -> EngineResult<()> {
    let target = match &plan.strategy {
        MergeStrategy::Reencode { target } => target,
        MergeStrategy::StreamCopy => {
            // Programming invariant: dispatcher only routes Reencode here.
            unreachable!("reencode::run invoked with StreamCopy strategy");
        }
    };

    let args = build_args(plan, target);
    tracing::info!(input_count = plan.inputs.len(), "spawning ffmpeg re-encode");

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

fn build_args(plan: &MergePlan, target: &CodecProfile) -> Vec<String> {
    let n = plan.inputs.len();
    let w = target.resolution.width.max(2);
    let h = target.resolution.height.max(2);
    let fps = if target.frame_rate_mhz > 0 {
        target.frame_rate_mhz as f64 / 1000.0
    } else {
        30.0
    };
    let fps_str = format!("{fps:.3}");
    let filter = build_filter(n, w, h, &fps_str);

    let (crf, preset) = quality_to_x264(plan.spec.quality);

    let mut args: Vec<String> = Vec::with_capacity(32 + n * 2);
    args.extend([
        "-y".into(),
        "-hide_banner".into(),
        "-loglevel".into(),
        "error".into(),
    ]);
    for input in &plan.inputs {
        // Per-input trim before the `-i` so ffmpeg seeks the demuxer (fast)
        // and emits only the requested range.
        if input.trim_in_us > 0 {
            args.push("-ss".into());
            args.push(format_us_to_ts(input.trim_in_us));
        }
        if input.trim_out_us > 0 {
            args.push("-to".into());
            args.push(format_us_to_ts(input.trim_out_us));
        }
        args.push("-i".into());
        args.push(input.path.to_string_lossy().into_owned());
    }
    args.push("-filter_complex".into());
    args.push(filter);
    args.extend([
        "-map".into(),
        "[outv]".into(),
        "-map".into(),
        "[outa]".into(),
        "-c:v".into(),
        "libx264".into(),
        "-preset".into(),
        preset.into(),
        "-crf".into(),
        crf.to_string(),
        "-pix_fmt".into(),
        "yuv420p".into(),
        "-c:a".into(),
        "aac".into(),
        "-b:a".into(),
        "192k".into(),
        "-progress".into(),
        "pipe:1".into(),
        "-nostats".into(),
    ]);
    args.push(plan.spec.output_path.to_string_lossy().into_owned());
    args
}

/// Builds a normalize-then-concat filter_complex graph for `n` inputs.
fn build_filter(n: usize, w: u32, h: u32, fps_str: &str) -> String {
    let mut filter = String::new();
    for i in 0..n {
        let _ = write!(
            filter,
            "[{i}:v]scale={w}:{h}:force_original_aspect_ratio=decrease,\
             pad={w}:{h}:(ow-iw)/2:(oh-ih)/2,setsar=1,fps={fps_str},format=yuv420p[v{i}];"
        );
        let _ = write!(
            filter,
            "[{i}:a]aformat=sample_rates=48000:channel_layouts=stereo[a{i}];"
        );
    }
    for i in 0..n {
        let _ = write!(filter, "[v{i}][a{i}]");
    }
    let _ = write!(filter, "concat=n={n}:v=1:a=1[outv][outa]");
    filter
}

fn quality_to_x264(q: Quality) -> (u32, &'static str) {
    match q {
        Quality::Lossless => (18, "slow"),
        Quality::High => (20, "medium"),
        Quality::Balanced => (23, "fast"),
    }
}

/// Format microseconds as `HH:MM:SS.sss` for ffmpeg's -ss / -to.
fn format_us_to_ts(us: i64) -> String {
    let us = us.max(0) as u64;
    let total_ms = us / 1000;
    let ms = total_ms % 1000;
    let total_s = total_ms / 1000;
    let s = total_s % 60;
    let total_m = total_s / 60;
    let m = total_m % 60;
    let h = total_m / 60;
    format!("{h:02}:{m:02}:{s:02}.{ms:03}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_for_two_inputs_has_both_normalize_chains_and_concat_n_2() {
        let f = build_filter(2, 3840, 2160, "29.970");
        assert!(f.contains("[0:v]scale=3840:2160"));
        assert!(f.contains("[1:v]scale=3840:2160"));
        assert!(f.contains("[0:a]aformat=sample_rates=48000:channel_layouts=stereo[a0]"));
        assert!(f.contains("[1:a]aformat=sample_rates=48000:channel_layouts=stereo[a1]"));
        assert!(f.contains("[v0][a0][v1][a1]concat=n=2:v=1:a=1[outv][outa]"));
        assert!(f.contains("fps=29.970"));
    }

    #[test]
    fn quality_mappings() {
        assert_eq!(quality_to_x264(Quality::Lossless), (18, "slow"));
        assert_eq!(quality_to_x264(Quality::High), (20, "medium"));
        assert_eq!(quality_to_x264(Quality::Balanced), (23, "fast"));
    }
}
