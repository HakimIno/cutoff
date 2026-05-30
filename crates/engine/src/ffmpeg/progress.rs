use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::ChildStdout;

use crate::traits::{ProgressEvent, ProgressSink};
use crate::EngineResult;

/// Parse FFmpeg's `-progress pipe:1` key/value stream and forward events
/// onto the supplied sink. The `total_duration_secs` value is used to
/// normalize progress into the 0.0..=1.0 range.
pub async fn pump(
    stdout: ChildStdout,
    total_duration_secs: f64,
    sink: ProgressSink,
) -> EngineResult<()> {
    let mut reader = BufReader::new(stdout).lines();
    while let Some(line) = reader.next_line().await? {
        if let Some(rest) = line.strip_prefix("out_time_us=") {
            if let Ok(us) = rest.trim().parse::<u64>() {
                let processed = us as f64 / 1_000_000.0;
                let fraction = if total_duration_secs > 0.0 {
                    (processed / total_duration_secs).min(1.0) as f32
                } else {
                    0.0
                };
                let _ = sink.send(ProgressEvent {
                    fraction,
                    processed_secs: processed,
                    eta_secs: None,
                });
            }
        }
    }
    Ok(())
}
