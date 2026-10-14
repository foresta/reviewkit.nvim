use std::path::Path;
use std::process::{Command, Stdio};

/// First line of `<tool> --version`, or `None` when the tool cannot be run.
/// Output is read lossily: a tool's banner is never trusted to be UTF-8.
pub fn version(path: &Path) -> Option<String> {
    let output = Command::new(path).arg("--version").stdin(Stdio::null()).output().ok()?;
    if !output.status.success() {
        log::debug!("{} --version exited with {}", path.display(), output.status);
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout
        .lines()
        .next()
        .map(|line| line.trim().to_string())
        .filter(|line| !line.is_empty())
}
