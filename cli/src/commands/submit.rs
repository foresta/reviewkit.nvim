use clap::{Args, Subcommand};

use crate::context::Context;
use crate::core::apply;
use crate::core::time;
use crate::envelope::Response;
use crate::error::{Error, ErrorCode, Result};
use crate::model::{SCHEMA_REVISION, SinkResult, SubmitOptions};
use crate::sink;
use crate::store::{QueuedApply, Store};

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
        /// Delivered comments leave the queue (the client sink decides).
        #[arg(long)]
        consume: bool,
        /// Only record results still waiting for the session; stdin is not read.
        #[arg(long, conflicts_with = "consume")]
        drain: bool,
    },
}

pub fn run(ctx: &Context, args: SubmitArgs) -> Result<Response> {
    let store = Store::new(ctx.state_dir.clone());
    if let Some(SubmitSubcommand::Apply {
        session,
        consume,
        drain,
    }) = args.command
    {
        if drain {
            let (session, ()) = store.update_if_changed(&session, |_| Ok(()))?;
            return Response::with_session(serde_json::Value::Null, &session);
        }
        let mut result: SinkResult = super::stdin_json_lenient()?;
        // Also the key that keeps a replayed result from being recorded twice,
        // so a sink's own (possibly coarse) time is not trusted for it.
        result.at = time::now();
        store.queue_apply(
            &session,
            &QueuedApply {
                revision: SCHEMA_REVISION,
                consume,
                result: result.clone(),
            },
        )?;
        return match store.update(&session, |_| Ok(())) {
            Ok((session, ())) => {
                let applied = session
                    .last_submit
                    .as_ref()
                    .filter(|last| last.at == result.at)
                    .map(|last| last.result.clone())
                    .unwrap_or_else(|| SinkResult {
                        summary: apply::summary(&result),
                        ..result
                    });
                Response::with_session(applied, &session)
            }
            Err(err) if err.code == ErrorCode::ELocked => {
                let session = store.load(&session)?;
                result.summary = format!(
                    "{}; recorded once the session is free (another submit is running)",
                    apply::summary(&result)
                );
                Response::with_session(result, &session)
            }
            Err(err) => Err(err),
        };
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
        let session = store.load_with_queued(&session_id)?;
        let preview = sink.preview(ctx, &session, &opts)?;
        // The client renders comment text from its own copy of the session,
        // which may trail the one this preview (and its digest) was made from.
        let stored = store.load(&session_id)?;
        return Response::with_session(serde_json::json!({ "preview": preview }), &stored);
    }
    let (session, result) = sink.submit(ctx, &store, &session_id, &opts)?;
    Response::with_session(result, &session)
}
