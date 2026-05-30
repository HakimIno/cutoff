use std::time::Duration;
use video_merger_core::domain::Clip;

pub struct ClipRow {
    pub id: String,
    pub name: String,
    pub duration: String,
    pub resolution: String,
    pub duration_secs: f32,
}

impl From<&Clip> for ClipRow {
    fn from(clip: &Clip) -> Self {
        Self {
            id: clip.id.to_string(),
            name: clip
                .path
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default(),
            duration: format_duration(clip.info.duration),
            resolution: format!(
                "{}x{}",
                clip.info.profile.resolution.width, clip.info.profile.resolution.height
            ),
            duration_secs: clip.info.duration.as_secs_f32(),
        }
    }
}

pub fn format_duration(d: Duration) -> String {
    let total = d.as_secs();
    let h = total / 3600;
    let m = (total % 3600) / 60;
    let s = total % 60;
    if h > 0 {
        format!("{h:02}:{m:02}:{s:02}")
    } else {
        format!("{m:02}:{s:02}")
    }
}

pub fn format_us(us: i64) -> String {
    let d = Duration::from_micros(us.max(0) as u64);
    format_duration(d)
}
