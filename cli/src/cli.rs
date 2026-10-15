use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

use crate::context::{Context, Tools};
use crate::error::{Error, Result};
use crate::store::{StateDir, env_non_empty};

#[derive(Debug, Parser)]
#[command(name = "reviewkit", version, about, disable_help_subcommand = true)]
pub struct Cli {
    #[command(flatten)]
    pub global: GlobalArgs,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Args)]
pub struct GlobalArgs {
    /// Directory for sessions and mirrors.
    /// Defaults to $REVIEWKIT_STATE_DIR, then $XDG_STATE_HOME/reviewkit, then ~/.local/state/reviewkit.
    #[arg(long, global = true, value_name = "DIR")]
    pub state_dir: Option<PathBuf>,

    /// Directory to resolve the repository from. Defaults to the current directory.
    #[arg(long, global = true, value_name = "DIR")]
    pub cwd: Option<PathBuf>,

    /// Log level: error, warn, info, debug, trace. Defaults to $REVIEWKIT_LOG, then warn.
    #[arg(long, global = true, value_name = "LEVEL")]
    pub log_level: Option<log::LevelFilter>,

    /// Append logs to this file instead of stderr.
    #[arg(long, global = true, value_name = "FILE")]
    pub log_file: Option<PathBuf>,
}

impl GlobalArgs {
    /// Flag > `$REVIEWKIT_*` > built-in default, for every flag, in one place.
    /// clap's own `env` / `default_value` are not used: `env` treats an empty
    /// variable as a value, and the state-dir fallback is a chain, not a value.
    pub fn resolve(&self) -> Result<Context> {
        let cwd = match &self.cwd {
            Some(dir) => dir.clone(),
            None => std::env::current_dir()?,
        };
        if !cwd.is_dir() {
            return Err(Error::invalid(format!("--cwd is not a directory: {}", cwd.display())));
        }

        let state_dir = StateDir::resolve(self.state_dir.clone())?;

        let log_level = self
            .log_level
            .or_else(|| env_non_empty("REVIEWKIT_LOG")?.to_str()?.parse().ok())
            .unwrap_or(log::LevelFilter::Warn);

        Ok(Context {
            state_dir,
            cwd,
            log_level,
            log_file: self.log_file.clone(),
            tools: Tools {
                git: resolve_tool("REVIEWKIT_GIT", "git"),
                gh: resolve_tool("REVIEWKIT_GH", "gh"),
                herdr: resolve_tool("REVIEWKIT_HERDR", "herdr"),
            },
        })
    }
}

/// An explicit override wins even when the file does not exist, so that a
/// broken override surfaces in `health` instead of silently falling back.
fn resolve_tool(env: &str, name: &str) -> Option<PathBuf> {
    match env_non_empty(env) {
        Some(path) => Some(PathBuf::from(path)),
        None => which::which(name).ok(),
    }
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Print the binary version and the schema version of the JSON contract.
    Version,
    /// Check external tools, gh authentication and the state directory.
    Health,
    /// Print the JSON Schema of every type in the contract.
    Schema,
    /// Open, resume, list and close review sessions.
    #[command(subcommand)]
    Session(crate::commands::session::SessionCommand),
    /// Inspect the targets of a session.
    #[command(subcommand)]
    Target(crate::commands::target::TargetCommand),
    /// Read file contents of a session's base side.
    #[command(subcommand)]
    File(crate::commands::file::FileCommand),
    /// Queue, edit and list review comments.
    #[command(subcommand)]
    Comment(crate::commands::comment::CommentCommand),
    /// Set or clear the review decision.
    #[command(subcommand)]
    Decision(crate::commands::decision::DecisionCommand),
}
