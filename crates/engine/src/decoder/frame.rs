use std::sync::Arc;

#[derive(Clone)]
pub struct DecodedFrame {
    pub width: u32,
    pub height: u32,
    pub pts_us: i64,
    pub rgba: Arc<[u8]>,
}

impl std::fmt::Debug for DecodedFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DecodedFrame")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("pts_us", &self.pts_us)
            .field("rgba_len", &self.rgba.len())
            .finish()
    }
}
