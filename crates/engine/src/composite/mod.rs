//! Multi-track compositor.
//!
//! Given a set of [`Track`]s, the compositor computes which clips are
//! active at any point in time and produces a [`RenderPlan`] that the
//! FFmpeg engine can translate into a `-filter_complex` graph using
//! `overlay` (video) and `amix` (audio) filters.
//!
//! # Z-order
//! Video tracks are layered bottom-to-top: V1 is the base, V2 is overlaid
//! on top via alpha composite.
//!
//! # Audio mixing
//! All non-muted audio tracks are mixed together with `amix`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use video_merger_core::domain::{
    ExportSpec, Project, ResolvedTransform, Track, TrackClip, TrackKind, TransformKeys,
};
use video_merger_core::services::{MergeInput, MergePlan, MergeStrategy};

/// A single time-segment where a clip is active on a track.
#[derive(Debug, Clone)]
pub struct RenderSegment {
    /// Index into `RenderPlan.inputs`.
    pub input_idx: usize,
    /// Absolute timeline start (µs).
    pub start_us: i64,
    /// Absolute timeline end (µs).
    pub end_us: i64,
    /// Track kind (Video / Audio).
    pub track_kind: TrackKind,
    /// Z-order: higher means on top. V1=0, V2=1.
    pub z_order: usize,
    /// Trim-in within the source file (µs).
    pub trim_in_us: i64,
    /// Trim-out within the source file (µs).
    pub trim_out_us: i64,
    /// Track index in the project.
    pub track_idx: usize,
    /// Whether this track is muted.
    pub muted: bool,
    /// Resolved transform sampled at the clip's start (rel=0). Carries opacity,
    /// per-axis scale, rotation, flips and crop. Used directly for non-animated
    /// clips and as the constant for non-animated channels of animated clips.
    pub transform: ResolvedTransform,
    /// Per-channel keyframe tracks (clip-relative time). When a channel is
    /// animated the filter graph emits a time-varying expression instead of the
    /// constant in `transform`.
    pub transform_keys: TransformKeys,
}

/// Complete render plan for multi-track export.
#[derive(Debug, Clone)]
pub struct RenderPlan {
    /// De-duplicated list of source files.
    pub inputs: Vec<MergeInput>,
    /// Video segments sorted by (z_order, start_us).
    pub video_segments: Vec<RenderSegment>,
    /// Audio segments sorted by (z_order, start_us).
    pub audio_segments: Vec<RenderSegment>,
    /// Total project duration (µs).
    pub total_duration_us: i64,
}

/// Build a `RenderPlan` from a `Project`.
pub fn build_render_plan(project: &Project) -> RenderPlan {
    let mut inputs: Vec<MergeInput> = Vec::new();
    let mut input_map: HashMap<PathBuf, usize> = HashMap::new();
    let mut video_segments = Vec::new();
    let mut audio_segments = Vec::new();

    for (track_idx, track) in project.tracks.iter().enumerate() {
        let z_order = compute_z_order(track);

        for tc in &track.clips {
            let input_idx = *input_map
                .entry(tc.clip.path.clone())
                .or_insert_with(|| {
                    let idx = inputs.len();
                    inputs.push(MergeInput {
                        path: tc.clip.path.clone(),
                        trim_in_us: tc.clip.trim_in_us(),
                        trim_out_us: tc.clip.trim_out_us(),
                    });
                    idx
                });

            // For multi-track, each clip placement gets its own input entry
            // because the same source file may be used at different trim
            // points on different tracks. Remap to a unique input.
            let actual_idx = inputs.len();
            inputs.push(MergeInput {
                path: tc.clip.path.clone(),
                trim_in_us: tc.clip.trim_in_us(),
                trim_out_us: tc.clip.trim_out_us(),
            });
            // Remove the shared mapping optimization — each segment
            // references its own input for unique -ss/-to positioning.
            let _ = input_idx; // suppress unused

            let segment = RenderSegment {
                input_idx: actual_idx,
                start_us: tc.start_us,
                end_us: tc.end_us(),
                track_kind: track.kind,
                z_order,
                trim_in_us: tc.clip.trim_in_us(),
                trim_out_us: tc.clip.trim_out_us(),
                track_idx,
                muted: track.muted,
                transform: tc.clip.resolved_transform(0),
                transform_keys: tc.clip.transform_keys.clone(),
            };

            match track.kind {
                TrackKind::Video => video_segments.push(segment),
                TrackKind::Audio => audio_segments.push(segment),
            }
        }
    }

    // Sort: lower z_order first (V1 before V2), then by start time.
    video_segments.sort_by(|a, b| a.z_order.cmp(&b.z_order).then(a.start_us.cmp(&b.start_us)));
    audio_segments.sort_by(|a, b| a.z_order.cmp(&b.z_order).then(a.start_us.cmp(&b.start_us)));

    // Fix: re-number inputs to remove the initial mapped entry.
    // Actually, we need each segment to be its own input. Let's rebuild clean.
    let mut clean_inputs = Vec::new();
    let mut video_clean = Vec::new();
    let mut audio_clean = Vec::new();

    // Collect all segments and assign fresh input indices.
    let all_segs: Vec<(RenderSegment, bool)> = video_segments
        .into_iter()
        .map(|s| (s, true))
        .chain(audio_segments.into_iter().map(|s| (s, false)))
        .collect();

    for (mut seg, is_video) in all_segs {
        let idx = clean_inputs.len();
        clean_inputs.push(MergeInput {
            path: inputs[seg.input_idx].path.clone(),
            trim_in_us: seg.trim_in_us,
            trim_out_us: seg.trim_out_us,
        });
        seg.input_idx = idx;
        if is_video {
            video_clean.push(seg);
        } else {
            audio_clean.push(seg);
        }
    }

    RenderPlan {
        inputs: clean_inputs,
        video_segments: video_clean,
        audio_segments: audio_clean,
        total_duration_us: project.duration_us(),
    }
}

/// Convert a `RenderPlan` into a legacy `MergePlan` for backward compat.
///
/// This always uses the Reencode strategy because multi-track compositing
/// requires re-encoding (overlay/amix filters).
pub fn render_plan_to_merge_plan(
    render: &RenderPlan,
    spec: ExportSpec,
    target: video_merger_core::domain::CodecProfile,
) -> MergePlan {
    MergePlan {
        inputs: render.inputs.clone(),
        spec,
        strategy: MergeStrategy::Reencode { target },
        total_duration: Duration::from_micros(render.total_duration_us.max(0) as u64),
    }
}

/// Compute z-order for a track based on its name.
/// V1=0, V2=1; A1=0, A2=1; unknown=0.
fn compute_z_order(track: &Track) -> usize {
    match track.name.as_str() {
        "V1" | "A1" => 0,
        "V2" | "A2" => 1,
        _ => 0,
    }
}

/// At a given time `t_us`, return which clips are active on each video
/// track (for preview compositing).
pub fn active_clips_at(
    project: &Project,
    t_us: i64,
) -> Vec<(usize, &TrackClip)> {
    let mut result = Vec::new();
    for (track_idx, track) in project.tracks.iter().enumerate() {
        if track.muted {
            continue;
        }
        for tc in &track.clips {
            if t_us >= tc.start_us && t_us < tc.end_us() {
                result.push((track_idx, tc));
            }
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use video_merger_core::domain::{
        Clip, CodecProfile, MediaInfo, Project as CoreProject, Resolution, TrackClip as CoreTC,
    };

    fn make_clip(secs: f32) -> Clip {
        Clip::new(
            PathBuf::from(format!("/tmp/{}.mp4", secs)),
            MediaInfo {
                duration: Duration::from_secs_f32(secs),
                profile: CodecProfile {
                    video_codec: "h264".into(),
                    audio_codec: "aac".into(),
                    resolution: Resolution {
                        width: 1920,
                        height: 1080,
                    },
                    frame_rate_mhz: 30_000,
                    pixel_format: "yuv420p".into(),
                },
            },
        )
    }

    #[test]
    fn render_plan_separates_video_and_audio() {
        let mut proj = CoreProject::with_default_tracks();
        let v1 = proj.track_index_by_name("V1").unwrap();
        proj.tracks[v1].clips.push(CoreTC {
            clip: make_clip(5.0),
            start_us: 0,
        });
        let v2 = proj.track_index_by_name("V2").unwrap();
        proj.tracks[v2].clips.push(CoreTC {
            clip: make_clip(3.0),
            start_us: 1_000_000,
        });

        let plan = build_render_plan(&proj);
        assert_eq!(plan.video_segments.len(), 2);
        assert_eq!(plan.audio_segments.len(), 0);
        assert_eq!(plan.total_duration_us, 5_000_000);
    }

    #[test]
    fn z_order_v1_before_v2() {
        let mut proj = CoreProject::with_default_tracks();
        let v1 = proj.track_index_by_name("V1").unwrap();
        let v2 = proj.track_index_by_name("V2").unwrap();
        proj.tracks[v1].clips.push(CoreTC {
            clip: make_clip(5.0),
            start_us: 0,
        });
        proj.tracks[v2].clips.push(CoreTC {
            clip: make_clip(5.0),
            start_us: 0,
        });

        let plan = build_render_plan(&proj);
        // V1 (z=0) should come before V2 (z=1).
        assert_eq!(plan.video_segments[0].z_order, 0);
        assert_eq!(plan.video_segments[1].z_order, 1);
    }
}
