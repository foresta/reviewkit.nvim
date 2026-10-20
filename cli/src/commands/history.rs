use clap::Subcommand;

use crate::context::Context;
use crate::core::{anchor, comments, files};
use crate::envelope::Response;
use crate::error::Result;
use crate::ext::git::SystemGit;
use crate::store::Store;

#[derive(Debug, Subcommand)]
pub enum HistoryCommand {
    /// Print the comments that left the queue, newest first.
    List {
        #[arg(long, value_name = "ID")]
        session: String,
    },
    /// Put a comment back at the end of the queue, keeping where it went.
    Restore {
        #[arg(long, value_name = "ID")]
        session: String,
        #[arg(long, value_name = "CID")]
        id: String,
    },
    /// Forget the history.
    Clear {
        #[arg(long, value_name = "ID")]
        session: String,
    },
}

pub fn run(ctx: &Context, command: HistoryCommand) -> Result<Response> {
    let store = Store::new(ctx.state_dir.clone());
    match command {
        HistoryCommand::List { session } => {
            let mut history = store.load(&session)?.history;
            history.reverse();
            Response::new(history)
        }
        HistoryCommand::Restore { session, id } => {
            let git = SystemGit::new(ctx.tools.git.as_deref())?;
            let (session, ()) = store.update(&session, |s| {
                comments::restore(s, &id)?;
                // Its line and commentability were for the diff it left.
                let repo = files::object_repo(&git, s);
                anchor::reanchor(s, &repo)
            })?;
            Response::with_session(serde_json::Value::Null, &session)
        }
        HistoryCommand::Clear { session } => {
            let (session, removed) = store.update(&session, |s| Ok(std::mem::take(&mut s.history).len()))?;
            Response::with_session(serde_json::json!({ "removed": removed }), &session)
        }
    }
}
