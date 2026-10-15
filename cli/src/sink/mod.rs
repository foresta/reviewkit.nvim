pub mod github;
pub mod herdr;

use serde::{Deserialize, Serialize};

use crate::context::Context;

#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SinkInfo {
    pub name: String,
    pub available: bool,
    pub reason: Option<String>,
}

/// Only sinks that exist in this build are listed; a client never has to
/// guess which names `submit --sink` accepts.
pub fn list(ctx: &Context) -> Vec<SinkInfo> {
    let herdr = ctx.tools.herdr.as_deref().is_some_and(std::path::Path::is_file);
    let gh = ctx.tools.gh.as_deref().is_some_and(std::path::Path::is_file);
    vec![
        SinkInfo {
            name: "herdr".into(),
            available: herdr,
            reason: (!herdr).then(|| "herdr is not installed or not on PATH".to_string()),
        },
        SinkInfo {
            name: "github".into(),
            available: gh,
            reason: (!gh).then(|| "gh is not installed or not on PATH".to_string()),
        },
    ]
}
