use clap::{Args, Subcommand};

use crate::context::Context;
use crate::core::apply;
use crate::envelope::Response;
use crate::error::{Error, Result};
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
        // A client-side sink only copies or hands text over; nothing leaves
        // the queue unless the client says so.
        let (session, result) = store.update(&session, |s| apply::apply(s, result, false))?;
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
    let sink = sink::find(&sink_name)?;
    if args.dry_run {
        let session = store.load(&session_id)?;
        let preview = sink.preview(ctx, &session, &opts)?;
        return Response::new(serde_json::json!({ "preview": preview }));
    }
    let (session, result) = sink.submit(ctx, &store, &session_id, &opts)?;
    Response::with_session(result, &session)
}
