use clap::Subcommand;

use crate::context::Context;
use crate::envelope::Response;
use crate::error::Result;
use crate::sink;

#[derive(Debug, Subcommand)]
pub enum SinkCommand {
    /// List the sinks this build knows and whether they can be used.
    List,
    /// List what a sink can send to (for herdr: agents and panes).
    Targets {
        #[arg(long, value_name = "NAME")]
        sink: String,
    },
}

pub fn run(ctx: &Context, command: SinkCommand) -> Result<Response> {
    match command {
        SinkCommand::List => Response::new(sink::list(ctx)),
        SinkCommand::Targets { sink } => Response::new(sink::find(&sink)?.targets(ctx)?),
    }
}
