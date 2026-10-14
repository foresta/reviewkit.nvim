use clap::Subcommand;

use crate::context::Context;
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
}

pub fn run(ctx: &Context, command: TargetCommand) -> Result<Response> {
    let store = Store::new(ctx.state_dir.clone());
    match command {
        TargetCommand::List { session } => Response::new(store.load(&session)?.targets),
    }
}
