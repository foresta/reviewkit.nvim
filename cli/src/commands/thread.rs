use clap::Subcommand;

use crate::commands::session::refresh as refresh_session;
use crate::context::Context;
use crate::core::threads;
use crate::envelope::Response;
use crate::error::Result;
use crate::ext::git::SystemGit;
use crate::store::Store;

#[derive(Debug, Subcommand)]
pub enum ThreadCommand {
    /// Print the review threads of a pull request session.
    List {
        #[arg(long, value_name = "ID")]
        session: String,
        /// Fetch the pull request again first (the whole session is refreshed).
        #[arg(long)]
        refresh: bool,
    },
    /// Queue resolving a thread (or unresolving it, with --undo) for the next submit.
    Resolve {
        #[arg(long, value_name = "ID")]
        session: String,
        #[arg(long, value_name = "THREAD_ID")]
        thread: String,
        #[arg(long)]
        undo: bool,
    },
}

pub fn run(ctx: &Context, command: ThreadCommand) -> Result<Response> {
    let store = Store::new(ctx.state_dir.clone());
    match command {
        ThreadCommand::List {
            session,
            refresh: false,
        } => Response::new(store.load(&session)?.threads),
        ThreadCommand::List { session, refresh: true } => {
            let git = SystemGit::new(ctx.tools.git.as_deref())?;
            let session = refresh_session(ctx, &git, &store, &session)?;
            Response::with_session(&session.threads, &session)
        }
        ThreadCommand::Resolve { session, thread, undo } => {
            let (session, ()) = store.update(&session, |s| threads::set_resolved(s, &thread, !undo))?;
            Response::with_session(serde_json::Value::Null, &session)
        }
    }
}
