//! FFmpeg primitives — push, pull, process management.

use std::env;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use anyhow::{Context, bail};
use tokio::time::sleep;

/// Look up an environment variable or return a default.
pub fn env_or(key: &str, default: &str) -> String {
    env::var(key).unwrap_or_else(|_| default.to_string())
}

/// Kill a child process and wait for it to exit.
pub fn kill_and_wait(proc: &mut Child) {
    let _ = proc.kill();
    let _ = proc.wait();
}

/// Spawn ffmpeg to push a video to an ingest endpoint.
pub fn spawn_push(
    input_file: &Path,
    format_args: &[&str],
    push_url: &str,
) -> anyhow::Result<Child> {
    Command::new("ffmpeg")
        .args(["-re", "-i"])
        .arg(input_file)
        .args(format_args)
        .arg(push_url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("ffmpeg push failed")
}

/// Pull a stream via ffmpeg for `duration`, verifying video frames are received.
pub async fn pull_and_verify(url: &str, label: &str, duration: Duration) -> anyhow::Result<()> {
    let stderr_file = tempfile::NamedTempFile::new().context("create temp log file failed")?;

    let mut child = Command::new("ffmpeg")
        .args(["-i", url, "-f", "null", "/dev/null"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(stderr_file.as_file().try_clone()?)
        .spawn()
        .with_context(|| format!("ffmpeg pull ({label}) failed to start"))?;

    sleep(duration).await;

    let _ = child.kill();
    let _ = child.wait();

    let stderr = std::fs::read_to_string(stderr_file.path()).context("read pull log failed")?;

    if stderr.contains("frame=") {
        Ok(())
    } else if stderr.contains("Connection refused") || stderr.contains("Connection reset") {
        bail!("pull ({label}) failed: connection refused")
    } else {
        // Show the tail of stderr — ffmpeg errors appear at the end.
        let tail: String = stderr
            .chars()
            .rev()
            .take(2000)
            .collect::<String>()
            .chars()
            .rev()
            .collect();
        bail!("pull ({label}) failed: no video frames detected\nstderr (last 2000 chars):\n{tail}")
    }
}
