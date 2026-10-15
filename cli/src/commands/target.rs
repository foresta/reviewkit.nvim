use clap::Subcommand;

use crate::context::Context;
use crate::core::viewed;
use crate::envelope::Response;
use crate::error::Result;
use crate::store::Store;

#[derive(Debug, Subcommand)]
pub enum TargetCommand {
    /// Print the targets of a session.
    List {
        #[arg(long, value_name = "ID")]
        session: String,
    },
    /// Mark a target as viewed (or not, with --undo). Local to reviewkit.
    Viewed {
        #[arg(long, value_name = "ID")]
        session: String,
        #[arg(long, value_name = "PATH")]
        path: String,
        #[arg(long)]
        undo: bool,
    },
}

pub fn run(ctx: &Context, command: TargetCommand) -> Result<Response> {
    let store = Store::new(ctx.state_dir.clone());
    match command {
        TargetCommand::List { session } => Response::new(store.load(&session)?.targets),
        TargetCommand::Viewed { session, path, undo } => {
            let (session, ()) = store.update(&session, |s| viewed::set(s, &path, !undo))?;
            Response::with_session(serde_json::Value::Null, &session)
        }
    }
}
