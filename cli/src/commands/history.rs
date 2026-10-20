use clap::Subcommand;

use crate::context::Context;
use crate::envelope::Response;
use crate::error::Result;
use crate::store::Store;

#[derive(Debug, Subcommand)]
pub enum HistoryCommand {
    /// Print the comments that left the queue, newest first.
    List {
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
    }
}
