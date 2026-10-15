use clap::Subcommand;
use serde::Serialize;

use crate::context::Context;
use crate::envelope::Response;
use crate::error::{Error, Result};
use crate::ext::herdr::{self, Agent, Pane, SystemHerdr};
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

#[derive(Serialize)]
struct HerdrTargets {
    agents: Vec<Agent>,
    panes: Vec<Pane>,
}

pub fn run(ctx: &Context, command: SinkCommand) -> Result<Response> {
    match command {
        SinkCommand::List => Response::new(sink::list(ctx)),
        SinkCommand::Targets { sink } => match sink.as_str() {
            sink::herdr::NAME => {
                let herdr = SystemHerdr::new(ctx.tools.herdr.as_deref())?;
                Response::new(HerdrTargets {
                    agents: herdr::agents(&herdr)?,
                    panes: herdr::panes(&herdr)?,
                })
            }
            other => Err(Error::invalid(format!("unknown sink: {other}")).hint("run `reviewkit sink list`")),
        },
    }
}
