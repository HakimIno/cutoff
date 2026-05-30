use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CodecProfile {
    pub video_codec: String,
    pub audio_codec: String,
    pub resolution: Resolution,
    pub frame_rate_mhz: u32,
    pub pixel_format: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Resolution {
    pub width: u32,
    pub height: u32,
}

impl Resolution {
    pub const UHD_4K: Resolution = Resolution {
        width: 3840,
        height: 2160,
    };
}
