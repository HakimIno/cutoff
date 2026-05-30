use std::process::Stdio;

use tokio::process::{Child, Command};

use crate::EngineResult;

/// Spawn an ffmpeg-family process with stdout+stderr piped so they can be
/// parsed for progress and diagnostics.
pub fn spawn(bin: &str, args: &[String]) -> EngineResult<Child> {
    let child = Command::new(bin)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    Ok(child)
}
