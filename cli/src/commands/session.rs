use clap::{Args, Subcommand};
use serde::Serialize;

use crate::context::Context;
use crate::envelope::Response;
use crate::error::{Error, Result};
use crate::ext::git::{Repository, SystemGit};
use crate::model::{SessionSummary, Source};
use crate::source::local::{self, LocalSpec};
use crate::store::Store;

#[derive(Debug, Subcommand)]
pub enum SessionCommand {
    /// Open (or resume) a session and compute its targets.
    Open(OpenArgs),
    /// Print a session as it is stored.
    Get {
        #[arg(long, value_name = "ID")]
        session: String,
    },
    /// List stored sessions, newest first.
    List,
    /// Re-run the source: recompute targets and follow comments whose lines moved.
    Refresh {
        #[arg(long, value_name = "ID")]
        session: String,
        /// Accepted for forward compatibility; the whole session is recomputed regardless.
        #[arg(long, value_name = "PATH")]
        path: Vec<String>,
    },
    /// Close a session. With --purge, delete its state.
    Close {
        #[arg(long, value_name = "ID")]
        session: String,
        #[arg(long)]
        purge: bool,
    },
}

#[derive(Debug, Args)]
#[command(group(clap::ArgGroup::new("what").required(true).multiple(false)))]
pub struct OpenArgs {
    /// Review the working tree (or --head) against --base in the repository at --cwd.
    #[arg(long, group = "what")]
    pub local: bool,
    /// Resume an existing session by id.
    #[arg(long, group = "what", value_name = "ID")]
    pub session: Option<String>,
    /// Base revision for --local. Defaults to HEAD.
    #[arg(long, value_name = "REV", requires = "local")]
    pub base: Option<String>,
    /// Head revision for --local. Defaults to the working tree.
    #[arg(long, value_name = "REV", requires = "local")]
    pub head: Option<String>,
}

pub fn run(ctx: &Context, command: SessionCommand) -> Result<Response> {
    let store = Store::new(ctx.state_dir.clone());
    match command {
        SessionCommand::Open(args) => open(ctx, &store, args),
        SessionCommand::Get { session } => {
            let session = store.load(&session)?;
            Response::with_session(serde_json::Value::Null, &session)
        }
        SessionCommand::Refresh { session, .. } => {
            let git = SystemGit::new(ctx.tools.git.as_deref())?;
            let (session, ()) = store.update(&session, |s| refresh(&git, s))?;
            Response::with_session(serde_json::Value::Null, &session)
        }
        SessionCommand::List => {
            let summaries: Vec<SessionSummary> = store.list()?.iter().map(|s| s.summary()).collect();
            Response::new(summaries)
        }
        SessionCommand::Close { session, purge } => {
            if purge {
                store.remove(&session)?;
            } else if !store.exists(&session) {
                store.load(&session)?;
            }
            Response::new(Closed { closed: true })
        }
    }
}

fn refresh(git: &SystemGit, session: &mut crate::model::Session) -> Result<()> {
    match &session.source {
        Source::Local { worktree, .. } => {
            let repo = Repository::at(git, worktree.clone());
            local::refresh(session, &repo)
        }
        Source::Pr { .. } => Err(Error::invalid("refreshing PR sessions is not implemented yet")),
    }
}

#[derive(Serialize)]
struct Closed {
    closed: bool,
}

fn open(ctx: &Context, store: &Store, args: OpenArgs) -> Result<Response> {
    let git = SystemGit::new(ctx.tools.git.as_deref())?;
    let session = if let Some(id) = args.session {
        let (session, ()) = store.update(&id, |session| refresh(&git, session))?;
        session
    } else {
        let repo = Repository::discover(&git, &ctx.cwd)?;
        let spec = LocalSpec {
            base: args.base.unwrap_or_else(|| "HEAD".to_string()),
            head: args.head,
        };
        local::open(store, &repo, &spec)?
    };
    Response::with_session(serde_json::Value::Null, &session)
}
