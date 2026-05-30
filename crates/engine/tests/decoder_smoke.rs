use std::path::PathBuf;

use video_merger_engine::decoder::{extract_thumbnails, open, MediaDecoder};

fn test_video_path() -> Option<PathBuf> {
    let p = PathBuf::from("/tmp/cutoff_test_10s.mp4");
    p.exists().then_some(p)
}

#[test]
fn open_and_decode_first_frame() {
    let Some(p) = test_video_path() else {
        eprintln!("skipping: /tmp/cutoff_test_10s.mp4 not present");
        return;
    };
    let mut d = open(&p).expect("open");
    let meta = d.meta();
    assert!(meta.duration_us > 5_000_000, "duration_us={}", meta.duration_us);
    let frame = d.next_frame().expect("decode").expect("frame");
    assert!(frame.width > 0 && frame.height > 0);
    assert_eq!(
        frame.rgba.len(),
        (frame.width * frame.height * 4) as usize,
        "rgba buffer should be width*height*4 bytes"
    );
}

#[test]
fn seek_and_decode_returns_frame() {
    let Some(p) = test_video_path() else {
        eprintln!("skipping: /tmp/cutoff_test_10s.mp4 not present");
        return;
    };
    let mut d = open(&p).expect("open");
    d.seek_to_us(5_000_000).expect("seek");
    // Seek lands on the nearest keyframe at or before target. We just verify
    // that a frame comes out (frame-accurate stepping is layered on top in callers).
    let frame = d.next_frame().expect("decode").expect("frame");
    assert!(frame.width > 0 && frame.height > 0);
}

#[test]
fn extract_thumbnails_writes_files() {
    let Some(p) = test_video_path() else {
        eprintln!("skipping: /tmp/cutoff_test_10s.mp4 not present");
        return;
    };
    let out = std::env::temp_dir().join("cutoff_test_thumbs");
    let _ = std::fs::remove_dir_all(&out);
    let paths = extract_thumbnails(&p, &out, 5, 160, 90).expect("thumbs");
    assert_eq!(paths.len(), 5);
    for path in &paths {
        let meta = std::fs::metadata(path).expect("thumb file");
        assert!(meta.len() > 200, "thumbnail {:?} too small ({} bytes)", path, meta.len());
    }
}
