use clap::{Args, Subcommand};

use crate::context::Context;
use crate::core::apply;
use crate::envelope::Response;
use crate::error::{Error, Result};
use crate::ext::herdr::SystemHerdr;
use crate::model::{SinkResult, SubmitOptions};
use crate::sink;
use crate::store::Store;

/// `submit --sink X` sends; `submit apply` records a result produced by a
/// client-side sink through the same bookkeeping.
#[derive(Debug, Args)]
#[command(args_conflicts_with_subcommands = true, subcommand_negates_reqs = true)]
pub struct SubmitArgs {
    #[command(subcommand)]
    pub command: Option<SubmitSubcommand>,

    #[arg(long, value_name = "ID", required = true)]
    pub session: Option<String>,

    /// Which sink to send through (see `sink list`).
    #[arg(long, value_name = "NAME", required = true)]
    pub sink: Option<String>,

    /// Build the payload and resolve the target, but send nothing.
    #[arg(long)]
    pub dry_run: bool,

    /// Read SubmitOptions as JSON from stdin ("-"). Without it stdin is not touched.
    #[arg(long, value_name = "-")]
    pub options: Option<String>,
}

#[derive(Debug, Subcommand)]
pub enum SubmitSubcommand {
    /// Apply a SinkResult (from stdin) to the session.
    Apply {
        #[arg(long, value_name = "ID")]
        session: String,
    },
}

pub fn run(ctx: &Context, args: SubmitArgs) -> Result<Response> {
    let store = Store::new(ctx.state_dir.clone());
    if let Some(SubmitSubcommand::Apply { session }) = args.command {
        let result: SinkResult = super::stdin_json()?;
        let (session, result) = store.update(&session, |s| apply::apply(s, result))?;
        return Response::with_session(result, &session);
    }
    let (Some(session_id), Some(sink_name)) = (args.session, args.sink) else {
        return Err(Error::usage("--session and --sink are required"));
    };
    let opts: SubmitOptions = match args.options.as_deref() {
        None => SubmitOptions::default(),
        Some("-") => super::stdin_json()?,
        Some(other) => {
            return Err(Error::usage(format!(
                "--options accepts only `-` (stdin), got {other:?}"
            )));
        }
    };
    match sink_name.as_str() {
        sink::herdr::NAME => {
            let herdr = SystemHerdr::new(ctx.tools.herdr.as_deref())?;
            if args.dry_run {
                let session = store.load(&session_id)?;
                let preview = sink::herdr::preview(&session, &opts, &herdr)?;
                return Response::new(serde_json::json!({ "preview": preview }));
            }
            // The lock is held across the send so a concurrent edit cannot
            // slip into the queue between formatting and clearing it.
            let (session, result) = store.update(&session_id, |s| {
                let result = sink::herdr::submit(s, &opts, &herdr)?;
                apply::apply(s, result)
            })?;
            Response::with_session(result, &session)
        }
        other => Err(Error::invalid(format!("unknown sink: {other}")).hint("run `reviewkit sink list`")),
    }
}
