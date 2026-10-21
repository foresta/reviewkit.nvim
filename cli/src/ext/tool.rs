use std::path::Path;
use std::process::{Command, Stdio};

/// First line of `<tool> --version`, or `None` when the tool cannot be run.
/// Output is read lossily: a tool's banner is never trusted to be UTF-8.
pub fn version(path: &Path) -> Option<String> {
    version_of(Command::new(path))
}

pub fn version_of(mut command: Command) -> Option<String> {
    let output = command.arg("--version").stdin(Stdio::null()).output().ok()?;
    if !output.status.success() {
        log::debug!(
            "{} --version exited with {}",
            command.get_program().to_string_lossy(),
            output.status
        );
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout
        .lines()
        .next()
        .map(|line| line.trim().to_string())
        .filter(|line| !line.is_empty())
}

/// Only a tool that is absent or not executable is "missing"; an argument
/// list too long (a large prompt) or another spawn failure would otherwise
/// send the user to reinstall a tool that is there.
pub fn spawn_error(tool: &str, err: std::io::Error) -> crate::error::Error {
    use crate::error::{Error, ErrorCode};
    use std::io::ErrorKind::{NotFound, PermissionDenied};
    let code = if matches!(err.kind(), NotFound | PermissionDenied) {
        ErrorCode::EToolMissing
    } else {
        ErrorCode::ETool
    };
    // Read by the send paths: a process that never started delivered
    // nothing, so its failure is not "may have arrived".
    Error::tool(tool, code, format!("cannot run {tool}: {err}")).details(serde_json::json!({ "not_started": true }))
}

pub fn never_started(err: &crate::error::Error) -> bool {
    err.details["not_started"] == true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorCode;

    #[test]
    fn only_an_absent_tool_is_missing() {
        assert_eq!(
            spawn_error("x", std::io::Error::from(std::io::ErrorKind::NotFound)).code,
            ErrorCode::EToolMissing
        );
        let too_long = spawn_error("x", std::io::Error::from_raw_os_error(7));
        assert_eq!(too_long.code, ErrorCode::ETool, "E2BIG is not a missing tool");
        assert!(never_started(&too_long), "and nothing was sent");
    }
}
