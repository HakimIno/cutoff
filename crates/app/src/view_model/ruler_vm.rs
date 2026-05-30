//! Ruler-tick generator.
//!
//! Given a zoom factor (pixels per second) and a total duration, produce a
//! reasonable set of ticks with a major one every ~80px so labels never
//! collide. Minor ticks fall between majors as visual subdivisions.

#[derive(Debug, Clone, Copy)]
pub struct Tick {
    pub x_px: f32,
    pub label_secs: u32,
    pub major: bool,
}

/// Compute ticks for the visible timeline.
///
/// * `px_per_sec` — current zoom factor.
/// * `total_secs` — total timeline duration (clipped to ≥ 1 to always show
///   *some* axis even on an empty timeline).
/// * `target_label_px` — ideal horizontal distance between *labelled* ticks.
pub fn ticks(px_per_sec: f32, total_secs: f32, target_label_px: f32) -> Vec<Tick> {
    let px_per_sec = px_per_sec.max(0.5);
    let total_secs = total_secs.max(1.0);

    // Pick a step from a 1/2/5/10/15/30/60/300/600 ladder closest to target.
    let raw_step = (target_label_px / px_per_sec).max(1.0);
    let step_secs = snap_step(raw_step) as u32;
    let minor_div = if step_secs >= 60 { 6 } else { 5 };
    let minor_step = (step_secs as f32 / minor_div as f32).max(1.0);

    let mut out = Vec::new();
    let max_ticks = 4000usize;

    // Minor ticks
    let mut t = 0.0f32;
    while t <= total_secs && out.len() < max_ticks {
        let major = (t as u32) % step_secs == 0;
        out.push(Tick {
            x_px: t * px_per_sec,
            label_secs: t as u32,
            major,
        });
        t += minor_step;
    }
    out
}

fn snap_step(raw: f32) -> f32 {
    const LADDER: [f32; 12] = [
        1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 300.0, 600.0, 1800.0, 3600.0,
    ];
    for &s in &LADDER {
        if s >= raw {
            return s;
        }
    }
    *LADDER.last().unwrap()
}

pub fn format_label_secs(s: u32) -> String {
    let h = s / 3600;
    let m = (s % 3600) / 60;
    let sec = s % 60;
    if h > 0 {
        format!("{h}:{m:02}:{sec:02}")
    } else {
        format!("{m}:{sec:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ticks_for_short_clip_at_default_zoom() {
        // 10s clip at 6 px/sec → 60px total. With target ~80px per label,
        // a single label-ish tick is plausible.
        let t = ticks(6.0, 10.0, 80.0);
        assert!(!t.is_empty());
        assert!(t.iter().all(|x| x.x_px >= 0.0));
    }

    #[test]
    fn ticks_respect_decimation_cap() {
        let t = ticks(200.0, 100_000.0, 80.0);
        assert!(t.len() <= 4000);
    }

    #[test]
    fn label_format() {
        assert_eq!(format_label_secs(0), "0:00");
        assert_eq!(format_label_secs(65), "1:05");
        assert_eq!(format_label_secs(3725), "1:02:05");
    }
}
