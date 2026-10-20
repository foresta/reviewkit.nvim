pub mod github;
pub mod herdr;

use serde::{Deserialize, Serialize};

use crate::context::Context;
use crate::error::{Error, Result};
use crate::model::{Session, SinkResult, SinkTarget, Source, SubmitOptions, SubmitPreview};
use crate::store::Store;

/// `cli` sinks are built into this binary and `submit --sink` sends; `lua`
/// sinks are registered in the client and report through `submit apply`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum SinkRuntime {
    Cli,
    Lua,
}

#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SinkInfo {
    pub name: String,
    pub available: bool,
    pub reason: Option<String>,
    pub runtime: SinkRuntime,
}

/// A destination built into the CLI. Adding one is a file and a line in
/// `builtin`. A sink is handed the store, not a session: github holds the
/// lock across its request so that edits made meanwhile are not lost.
pub trait Sink: Sync {
    fn name(&self) -> &'static str;
    fn info(&self, ctx: &Context) -> SinkInfo;
    /// Whether what it delivered leaves the queue (decisions.md).
    fn consumes(&self, source: &Source, requested: Option<bool>) -> bool;
    fn targets(&self, _ctx: &Context) -> Result<Vec<SinkTarget>> {
        Err(Error::invalid(format!(
            "the {} sink has no targets to pick from",
            self.name()
        )))
    }
    fn preview(&self, ctx: &Context, session: &Session, opts: &SubmitOptions) -> Result<SubmitPreview>;
    fn submit(&self, ctx: &Context, store: &Store, id: &str, opts: &SubmitOptions) -> Result<(Session, SinkResult)>;
}

pub fn builtin() -> &'static [&'static dyn Sink] {
    &[&herdr::HerdrSink, &github::GithubSink]
}

pub fn find(name: &str) -> Result<&'static dyn Sink> {
    builtin()
        .iter()
        .copied()
        .find(|s| s.name() == name)
        .ok_or_else(|| Error::invalid(format!("unknown sink: {name}")).hint("run `reviewkit sink list`"))
}

/// Only sinks that exist in this build are listed; a client adds its own.
pub fn list(ctx: &Context) -> Vec<SinkInfo> {
    builtin().iter().map(|s| s.info(ctx)).collect()
}

fn tool_info(name: &str, tool: Option<&std::path::Path>, tool_name: &str) -> SinkInfo {
    let available = tool.is_some_and(std::path::Path::is_file);
    SinkInfo {
        name: name.into(),
        available,
        reason: (!available).then(|| format!("{tool_name} is not installed or not on PATH")),
        runtime: SinkRuntime::Cli,
    }
}
