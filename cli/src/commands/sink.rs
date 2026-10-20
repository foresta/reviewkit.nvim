use clap::Subcommand;

use crate::context::Context;
use crate::envelope::Response;
use crate::error::{Error, Result};
use crate::ext::herdr::{self, SystemHerdr};
use crate::model::SinkTarget;
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
        SinkCommand::Targets { sink } => match sink.as_str() {
            sink::herdr::NAME => {
                let herdr = SystemHerdr::new(ctx.tools.herdr.as_deref())?;
                let targets: Vec<SinkTarget> = herdr::panes(&herdr)?
                    .into_iter()
                    .map(|p| SinkTarget {
                        id: p.pane_id,
                        name: p.agent,
                        status: p.agent_status,
                        cwd: p.cwd,
                        title: p.terminal_title_stripped,
                    })
                    .collect();
                Response::new(targets)
            }
            other => Err(Error::invalid(format!("unknown sink: {other}")).hint("run `reviewkit sink list`")),
        },
    }
}
