use std::path::Path;
use std::process::{Command, Stdio};

use crate::context::Context;
use crate::envelope::Response;
use crate::error::Result;
use crate::model::health::{HealthInfo, StateDirStatus, ToolStatus, ToolsStatus};

pub fn run(ctx: &Context) -> Result<Response> {
    let gh = tool_status(ctx.tools.gh.as_deref());
    let gh_auth = ctx
        .tools
        .gh
        .as_deref()
        .filter(|_| gh.version.is_some())
        .map(gh_authenticated);
    Response::new(HealthInfo {
        tools: ToolsStatus {
            git: tool_status(ctx.tools.git.as_deref()),
            gh,
            herdr: tool_status(ctx.tools.herdr.as_deref()),
        },
        gh_auth,
        state_dir: StateDirStatus {
            writable: state_dir_writable(&ctx.state_dir),
            path: ctx.state_dir.clone(),
        },
    })
}

fn tool_status(path: Option<&Path>) -> ToolStatus {
    let version = path.and_then(|p| {
        let output = Command::new(p).arg("--version").stdin(Stdio::null()).output().ok()?;
        if !output.status.success() {
            log::debug!("{} --version exited with {}", p.display(), output.status);
            return None;
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        stdout
            .lines()
            .next()
            .map(|line| line.trim().to_string())
            .filter(|line| !line.is_empty())
    });
    ToolStatus {
        path: path.map(Path::to_path_buf),
        version,
    }
}

/// `gh auth token` only reads the local config, so health never waits on the
/// network (`gh auth status` would call the API).
fn gh_authenticated(gh: &Path) -> bool {
    Command::new(gh)
        .args(["auth", "token"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn state_dir_writable(dir: &Path) -> bool {
    if std::fs::create_dir_all(dir).is_err() {
        return false;
    }
    let probe = dir.join(format!(".write-probe-{}", std::process::id()));
    let ok = std::fs::write(&probe, b"").is_ok();
    let _ = std::fs::remove_file(&probe);
    ok
}
