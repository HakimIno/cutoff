use crate::domain::{CodecProfile, Playlist};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeStrategy {
    /// All clips share an identical codec profile — concat demuxer with `-c copy`.
    StreamCopy,
    /// At least one clip differs — re-encode to the target profile.
    Reencode { target: CodecProfile },
}

pub struct CompatibilityAnalyzer;

impl CompatibilityAnalyzer {
    pub fn analyze(playlist: &Playlist) -> Option<MergeStrategy> {
        let first = playlist.clips().first()?;
        let target = first.info.profile.clone();
        let all_match = playlist
            .clips()
            .iter()
            .all(|c| c.info.profile == target);

        Some(if all_match {
            MergeStrategy::StreamCopy
        } else {
            MergeStrategy::Reencode { target }
        })
    }
}
