use clap::Subcommand;

use super::stdin_json;
use crate::context::Context;
use crate::core::{comments, files};
use crate::envelope::Response;
use crate::error::Result;
use crate::ext::git::SystemGit;
use crate::model::{Comment, CommentInput, CommentPatch};
use crate::store::Store;

#[derive(Debug, Subcommand)]
pub enum CommentCommand {
    /// Queue a comment. Reads a CommentInput from stdin.
    Add {
        #[arg(long, value_name = "ID")]
        session: String,
    },
    /// Edit a queued comment. Reads a CommentPatch from stdin.
    Update {
        #[arg(long, value_name = "ID")]
        session: String,
        #[arg(long, value_name = "CID")]
        id: String,
    },
    /// Drop a queued comment.
    Remove {
        #[arg(long, value_name = "ID")]
        session: String,
        #[arg(long, value_name = "CID")]
        id: String,
    },
    /// Move delivered comments to the history.
    Clear {
        #[arg(long, value_name = "ID")]
        session: String,
        /// Required, so that clearing never empties the queue by accident.
        #[arg(long, required = true)]
        delivered: bool,
        /// Only comments delivered through this sink.
        #[arg(long, value_name = "NAME")]
        sink: Option<String>,
    },
    /// Print queued comments, optionally for one path.
    List {
        #[arg(long, value_name = "ID")]
        session: String,
        #[arg(long, value_name = "PATH")]
        path: Option<String>,
    },
}

pub fn run(ctx: &Context, command: CommentCommand) -> Result<Response> {
    let store = Store::new(ctx.state_dir.clone());
    match command {
        CommentCommand::Add { session } => {
            let input: CommentInput = stdin_json()?;
            let git = SystemGit::new(ctx.tools.git.as_deref())?;
            let (session, id) = store.update(&session, |s| {
                let repo = files::object_repo(&git, s);
                let file = files::read_side(s, &repo, input.side, &input.path)?;
                comments::add(s, input, file)
            })?;
            Response::with_session(serde_json::json!({ "id": id }), &session)
        }
        CommentCommand::Update { session, id } => {
            let patch: CommentPatch = stdin_json()?;
            let git = SystemGit::new(ctx.tools.git.as_deref())?;
            let (session, ()) = store.update(&session, |s| {
                let file = if comments::patch_moves_line(&patch) {
                    let index = comments::find(s, &id)?;
                    let (side, path) = (s.comments[index].side, s.comments[index].path.clone());
                    let repo = files::object_repo(&git, s);
                    files::read_side(s, &repo, side, &path)?
                } else {
                    None
                };
                comments::update(s, &id, patch, file)
            })?;
            Response::with_session(serde_json::Value::Null, &session)
        }
        CommentCommand::Remove { session, id } => {
            let (session, _) = store.update(&session, |s| comments::remove(s, &id))?;
            Response::with_session(serde_json::Value::Null, &session)
        }
        CommentCommand::Clear { session, sink, .. } => {
            let (session, moved) = store.update(&session, |s| Ok(comments::clear_delivered(s, sink.as_deref())))?;
            Response::with_session(serde_json::json!({ "moved": moved }), &session)
        }
        CommentCommand::List { session, path } => {
            let session = store.load(&session)?;
            let comments: Vec<&Comment> = session
                .comments
                .iter()
                .filter(|c| path.as_deref().is_none_or(|p| c.path == p))
                .collect();
            Response::new(comments)
        }
    }
}
