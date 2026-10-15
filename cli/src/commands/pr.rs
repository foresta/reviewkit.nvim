use clap::Subcommand;

use crate::context::Context;
use crate::envelope::Response;
use crate::error::Result;
use crate::source::pr::read_pr_info;
use crate::store::Store;

#[derive(Debug, Subcommand)]
pub enum PrCommand {
    /// Print the pull request's conversation and checks (from pr.json).
    Show {
        #[arg(long, value_name = "ID")]
        session: String,
    },
}

pub fn run(ctx: &Context, command: PrCommand) -> Result<Response> {
    let store = Store::new(ctx.state_dir.clone());
    match command {
        PrCommand::Show { session } => {
            store.load(&session)?;
            let info = read_pr_info(&store.state_dir().pr_file(&session)?)?;
            Response::new(info)
        }
    }
}
