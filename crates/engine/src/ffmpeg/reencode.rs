use std::fmt::Write as _;

use tokio_util::sync::CancellationToken;
use video_merger_core::domain::{CodecProfile, Quality};
use video_merger_core::services::{MergePlan, MergeStrategy};

use crate::composite::RenderPlan;
use crate::ffmpeg::{cmd, progress};
use crate::traits::ProgressSink;
use crate::{EngineError, EngineResult};

/// Re-encoding merge path. Used when clips have mismatched codec profiles
/// or when multi-track compositing is needed.
///
/// For multi-track projects, the filter graph uses `overlay` for video
/// compositing (V2 on top of V1) and `amix` for mixing audio tracks.
/// For single-track (legacy), falls back to the simpler `concat` filter.
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

/// Run a multi-track export using a `RenderPlan` from the compositor.
pub async fn run_multitrack(
    bin: &str,
    render_plan: &RenderPlan,
    spec: &video_merger_core::domain::ExportSpec,
    target: &CodecProfile,
    quality: Quality,
    progress_tx: ProgressSink,
    cancel: CancellationToken,
) -> EngineResult<()> {
    let args = build_multitrack_args(render_plan, spec, target, quality);
    tracing::info!(
        input_count = render_plan.inputs.len(),
        video_segs = render_plan.video_segments.len(),
        audio_segs = render_plan.audio_segments.len(),
        "spawning ffmpeg multi-track re-encode"
    );

    let mut child = cmd::spawn(bin, &args)?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| EngineError::ProbeParse("ffmpeg stdout pipe missing".into()))?;

    let total_secs = render_plan.total_duration_us as f64 / 1_000_000.0;
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
/// Used for single-track (legacy) exports.
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

/// Build ffmpeg args for multi-track export using overlay + amix.
fn build_multitrack_args(
    render: &RenderPlan,
    spec: &video_merger_core::domain::ExportSpec,
    target: &CodecProfile,
    quality: Quality,
) -> Vec<String> {
    let w = target.resolution.width.max(2);
    let h = target.resolution.height.max(2);
    let fps = if target.frame_rate_mhz > 0 {
        target.frame_rate_mhz as f64 / 1000.0
    } else {
        30.0
    };
    let fps_str = format!("{fps:.3}");

    let (crf, preset) = quality_to_x264(quality);

    let mut args: Vec<String> = Vec::with_capacity(64);
    args.extend([
        "-y".into(),
        "-hide_banner".into(),
        "-loglevel".into(),
        "error".into(),
    ]);

    // Input declarations with per-input trim.
    for input in &render.inputs {
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

    // Build filter_complex.
    let filter = build_multitrack_filter(render, w, h, &fps_str);
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
    args.push(spec.output_path.to_string_lossy().into_owned());
    args
}

/// Build a multi-track filter_complex graph with overlay + amix.
///
/// Strategy:
/// 1. Create a black background canvas at target resolution/fps.
/// 2. For each video segment, normalize (scale+pad) and overlay at the
///    correct timeline position using `setpts`/`enable` timing.
/// 3. For audio, normalize and mix with `amix`.
fn build_multitrack_filter(
    render: &RenderPlan,
    w: u32,
    h: u32,
    fps_str: &str,
) -> String {
    let dur_secs = render.total_duration_us as f64 / 1_000_000.0;
    let mut filter = String::new();

    // Create a black background canvas.
    let _ = write!(
        filter,
        "color=c=black:s={w}x{h}:d={dur_secs:.6}:r={fps_str},format=yuv420p[base];"
    );

    // Video: normalize each segment and overlay.
    let active_video: Vec<_> = render
        .video_segments
        .iter()
        .enumerate()
        .filter(|(_, s)| !s.muted)
        .collect();

    let mut last_label = "base".to_string();
    for (seg_i, seg) in &active_video {
        let i = seg.input_idx;
        let delay_secs = seg.start_us as f64 / 1_000_000.0;
        let enable_start = seg.start_us as f64 / 1_000_000.0;
        let enable_end = seg.end_us as f64 / 1_000_000.0;

        // Normalize video.
        let _ = write!(
            filter,
            "[{i}:v]scale={w}:{h}:force_original_aspect_ratio=decrease,\
             pad={w}:{h}:(ow-iw)/2:(oh-ih)/2,setsar=1,fps={fps_str},\
             format=yuva420p,setpts=PTS+{delay_secs}/TB[ov{seg_i}];"
        );

        // Overlay onto accumulated canvas.
        let out_label = format!("cv{seg_i}");
        let _ = write!(
            filter,
            "[{last_label}][ov{seg_i}]overlay=0:0:enable='between(t,{enable_start:.6},{enable_end:.6})'[{out_label}];"
        );
        last_label = out_label;
    }

    // Map final video label to [outv].
    if active_video.is_empty() {
        // No video segments — use the black canvas.
        let _ = write!(filter, "[base]null[outv];");
    } else {
        // Rename last overlay output.
        let _ = write!(filter, "[{last_label}]null[outv];");
    }

    // Audio: normalize and mix.
    let active_audio: Vec<_> = render
        .audio_segments
        .iter()
        .enumerate()
        .filter(|(_, s)| !s.muted)
        .collect();

    if active_audio.is_empty() {
        // Generate silence.
        let _ = write!(
            filter,
            "anullsrc=r=48000:cl=stereo:d={dur_secs:.6}[outa]"
        );
    } else if active_audio.len() == 1 {
        let (_, seg) = &active_audio[0];
        let i = seg.input_idx;
        let delay_secs = seg.start_us as f64 / 1_000_000.0;
        let _ = write!(
            filter,
            "[{i}:a]aformat=sample_rates=48000:channel_layouts=stereo,\
             adelay={delay_ms}|{delay_ms}[outa]",
            delay_ms = (delay_secs * 1000.0) as i64
        );
    } else {
        // Normalize each audio stream.
        for (seg_i, seg) in &active_audio {
            let i = seg.input_idx;
            let delay_ms = (seg.start_us as f64 / 1000.0) as i64;
            let _ = write!(
                filter,
                "[{i}:a]aformat=sample_rates=48000:channel_layouts=stereo,\
                 adelay={delay_ms}|{delay_ms}[oa{seg_i}];"
            );
        }
        // Mix all audio.
        for (seg_i, _) in &active_audio {
            let _ = write!(filter, "[oa{seg_i}]");
        }
        let n = active_audio.len();
        let _ = write!(
            filter,
            "amix=inputs={n}:duration=longest:dropout_transition=0[outa]"
        );
    }

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

    #[test]
    fn multitrack_filter_generates_overlay() {
        use crate::composite::build_render_plan;
        use video_merger_core::domain::{
            Clip, CodecProfile, MediaInfo, Project, Resolution, TrackClip,
        };
        use std::path::PathBuf;
        use std::time::Duration;

        let mut proj = Project::with_default_tracks();
        let v1 = proj.track_index_by_name("V1").unwrap();
        proj.tracks[v1].clips.push(TrackClip {
            clip: Clip::new(
                PathBuf::from("/tmp/base.mp4"),
                MediaInfo {
                    duration: Duration::from_secs(10),
                    profile: CodecProfile {
                        video_codec: "h264".into(),
                        audio_codec: "aac".into(),
                        resolution: Resolution { width: 1920, height: 1080 },
                        frame_rate_mhz: 30_000,
                        pixel_format: "yuv420p".into(),
                    },
                },
            ),
            start_us: 0,
        });

        let plan = build_render_plan(&proj);
        let filter = build_multitrack_filter(&plan, 1920, 1080, "30.000");
        assert!(filter.contains("color=c=black:s=1920x1080"));
        assert!(filter.contains("overlay"));
        assert!(filter.contains("[outv]"));
    }
}
