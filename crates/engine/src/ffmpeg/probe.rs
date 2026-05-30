use std::path::Path;
use std::time::Duration;

use serde::Deserialize;
use tokio::process::Command;
use video_merger_core::domain::{CodecProfile, MediaInfo, Resolution};

use crate::{EngineError, EngineResult};

/// Run `ffprobe -print_format json -show_streams -show_format`.
pub async fn probe(bin: &str, path: &Path) -> EngineResult<MediaInfo> {
    let output = Command::new(bin)
        .args([
            "-v",
            "error",
            "-print_format",
            "json",
            "-show_streams",
            "-show_format",
        ])
        .arg(path)
        .output()
        .await?;

    if !output.status.success() {
        return Err(EngineError::NonZeroExit(output.status.code().unwrap_or(-1)));
    }

    parse_ffprobe_json(&output.stdout)
}

#[derive(Deserialize)]
struct ProbeOutput {
    #[serde(default)]
    streams: Vec<Stream>,
    #[serde(default)]
    format: Format,
}

#[derive(Deserialize)]
struct Stream {
    codec_type: String,
    #[serde(default)]
    codec_name: String,
    #[serde(default)]
    width: u32,
    #[serde(default)]
    height: u32,
    #[serde(default)]
    pix_fmt: String,
    #[serde(default)]
    r_frame_rate: String,
}

#[derive(Default, Deserialize)]
struct Format {
    #[serde(default)]
    duration: String,
}

fn parse_ffprobe_json(bytes: &[u8]) -> EngineResult<MediaInfo> {
    let parsed: ProbeOutput = serde_json::from_slice(bytes)
        .map_err(|e| EngineError::ProbeParse(e.to_string()))?;

    let video = parsed
        .streams
        .iter()
        .find(|s| s.codec_type == "video")
        .ok_or_else(|| EngineError::ProbeParse("no video stream".into()))?;

    let audio_codec = parsed
        .streams
        .iter()
        .find(|s| s.codec_type == "audio")
        .map(|s| s.codec_name.clone())
        .unwrap_or_default();

    let frame_rate_mhz = parse_rational_mhz(&video.r_frame_rate).unwrap_or(0);
    let duration_secs: f64 = parsed.format.duration.parse().unwrap_or(0.0);

    Ok(MediaInfo {
        duration: Duration::from_secs_f64(duration_secs.max(0.0)),
        profile: CodecProfile {
            video_codec: video.codec_name.clone(),
            audio_codec,
            resolution: Resolution {
                width: video.width,
                height: video.height,
            },
            frame_rate_mhz,
            pixel_format: video.pix_fmt.clone(),
        },
    })
}

/// Parses ffprobe's `r_frame_rate` ("30000/1001", "30/1", "0/0") into a
/// fixed-point frame rate scaled by 1000 (so 29.97 fps -> 29970).
fn parse_rational_mhz(s: &str) -> Option<u32> {
    let (num, den) = s.split_once('/')?;
    let n: f64 = num.parse().ok()?;
    let d: f64 = den.parse().ok()?;
    if d == 0.0 {
        return None;
    }
    Some(((n / d) * 1000.0).round() as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
        "streams": [
            {"codec_type":"video","codec_name":"h264","width":3840,"height":2160,"pix_fmt":"yuv420p","r_frame_rate":"30000/1001"},
            {"codec_type":"audio","codec_name":"aac"}
        ],
        "format": {"duration":"12.345"}
    }"#;

    #[test]
    fn parses_full_4k_h264_sample() {
        let info = parse_ffprobe_json(SAMPLE.as_bytes()).unwrap();
        assert_eq!(info.profile.video_codec, "h264");
        assert_eq!(info.profile.audio_codec, "aac");
        assert_eq!(info.profile.resolution.width, 3840);
        assert_eq!(info.profile.resolution.height, 2160);
        assert_eq!(info.profile.frame_rate_mhz, 29970);
        assert!((info.duration.as_secs_f64() - 12.345).abs() < 1e-6);
    }

    #[test]
    fn errors_on_no_video_stream() {
        let json = br#"{"streams":[{"codec_type":"audio","codec_name":"aac"}],"format":{}}"#;
        assert!(parse_ffprobe_json(json).is_err());
    }
}
