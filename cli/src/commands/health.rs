use std::path::Path;
use std::process::Stdio;

use crate::context::Context;
use crate::envelope::Response;
use crate::error::Result;
use crate::model::health::{HealthInfo, StateDirStatus, ToolStatus, ToolsStatus};

pub fn run(ctx: &Context) -> Result<Response> {
    let gh = ToolStatus {
        path: ctx.tools.gh.clone(),
        version: ctx
            .tools
            .gh
            .as_deref()
            .and_then(|path| crate::ext::tool::version_of(crate::ext::gh::command(path))),
    };
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
            writable: state_dir_writable(ctx.state_dir.as_path()),
            path: ctx.state_dir.as_path().to_path_buf(),
        },
    })
}

fn tool_status(path: Option<&Path>) -> ToolStatus {
    ToolStatus {
        path: path.map(Path::to_path_buf),
        version: path.and_then(crate::ext::tool::version),
    }
}

/// `gh auth token` only reads the local config, so health never waits on the
/// network (`gh auth status` would call the API). It answers for gh's
/// default host (GH_HOST, else github.com): health has no session, so it
/// cannot know which host a review will use.
fn gh_authenticated(gh: &Path) -> bool {
    crate::ext::gh::command(gh)
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
