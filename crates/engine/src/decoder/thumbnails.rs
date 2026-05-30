use std::path::{Path, PathBuf};

use image::{ImageBuffer, Rgba};

use super::{ffmpeg_decoder::FfmpegMediaDecoder, DecoderError, DecoderResult, MediaDecoder};

/// Extract `count` evenly-spaced thumbnails from `source` into `out_dir` as
/// JPEG files named `0.jpg .. count-1.jpg`, downscaled to fit `max_w x max_h`.
///
/// Returns the absolute paths in order.
pub fn extract_thumbnails(
    source: &Path,
    out_dir: &Path,
    count: usize,
    max_w: u32,
    max_h: u32,
) -> DecoderResult<Vec<PathBuf>> {
    std::fs::create_dir_all(out_dir)?;
    let mut decoder = FfmpegMediaDecoder::open(source)?;
    let total_us = decoder.meta().duration_us.max(1_000_000);

    let mut paths = Vec::with_capacity(count);
    for i in 0..count {
        let t_us = total_us * (i as i64) / (count.max(1) as i64);
        decoder.seek_to_us(t_us)?;
        // Pull frames until we reach or pass target pts.
        let mut frame_opt = None;
        for _ in 0..200 {
            match decoder.next_frame()? {
                Some(f) if f.pts_us >= t_us => {
                    frame_opt = Some(f);
                    break;
                }
                Some(f) => frame_opt = Some(f),
                None => break,
            }
        }
        let Some(frame) = frame_opt else { continue };

        // Downscale into JPG via `image` crate.
        let src = ImageBuffer::<Rgba<u8>, Vec<u8>>::from_raw(
            frame.width,
            frame.height,
            frame.rgba.to_vec(),
        )
        .ok_or_else(|| DecoderError::ImageEncode("bad rgba buffer size".into()))?;
        let dyn_img = image::DynamicImage::ImageRgba8(src);
        let resized = dyn_img.thumbnail(max_w, max_h).to_rgb8();
        let path = out_dir.join(format!("{i}.jpg"));
        resized
            .save_with_format(&path, image::ImageFormat::Jpeg)
            .map_err(|e| DecoderError::ImageEncode(e.to_string()))?;
        paths.push(path);
    }

    Ok(paths)
}
