pub mod github;
pub mod herdr;

use crate::context::Context;
use crate::error::{Error, Result};
use crate::model::{Session, SinkResult, SinkTarget, Source, SubmitOptions, SubmitPreview};
use crate::store::Store;

pub use crate::model::{SinkInfo, SinkRuntime};

/// A destination built into the CLI. Adding one is a file and a line in
/// `builtin`. A sink is handed the store, not a session: github holds the
/// lock across its request so that edits made meanwhile are not lost.
pub trait Sink: Sync {
    fn name(&self) -> &'static str;
    fn info(&self, ctx: &Context) -> SinkInfo;
    /// Whether what it delivered leaves the queue (docs/design.md §3).
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

pub(crate) fn fence_for(text: &str) -> String {
    let longest = text.split(|c| c != '`').map(str::len).max().unwrap_or(0);
    "`".repeat(longest.max(2) + 1)
}
