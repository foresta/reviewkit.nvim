use clap::{Args, Subcommand};

use crate::context::Context;
use crate::core::apply;
use crate::envelope::Response;
use crate::error::{Error, Result};
use crate::ext::gh::{RepoRef, SystemGh};
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
        let (session, result) = store.update(&session, |s| apply::apply(s, result, None))?;
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
                apply::apply(s, result, opts.consume)
            })?;
            Response::with_session(result, &session)
        }
        sink::github::NAME => {
            let gh = SystemGh::new(ctx.tools.gh.as_deref())?;
            let session_dir = store.state_dir().session_dir(&session_id)?;
            // The preview does not reconcile an interrupted submit; it may list
            // comments the real submit will find already posted.
            if args.dry_run {
                let session = store.load(&session_id)?;
                let preview = sink::github::preview(&session)?;
                return Response::new(serde_json::json!({ "preview": preview }));
            }
            // Phase 1: an interrupted earlier submit is reconciled and saved
            // on its own, so its record can go before a new one is written.
            let pending = sink::github::pending_path(&session_dir);
            let mut recovered = None;
            if pending.exists() {
                let (_, found) = store.update(&session_id, |s| {
                    let repo = repo_ref_of(s)?;
                    let found = sink::github::reconcile_pending(s, &gh, &repo, &session_dir)?;
                    if let Some(found) = &found {
                        apply::apply(s, found.clone(), opts.consume)?;
                    }
                    Ok(found)
                })?;
                let _ = std::fs::remove_file(&pending);
                recovered = found;
            }
            // Phase 2: the lock is held across the request. The answer is
            // matched to comments by id, and a concurrent update or remove of
            // a planned comment (or a decision change) would otherwise be
            // lost or cleared by it. Other commands see E_LOCKED meanwhile.
            let outcome = store.update(&session_id, |s| {
                let repo = repo_ref_of(s)?;
                let mut result = sink::github::submit(s, &gh, &repo, &session_dir)?;
                if let Some(recovered) = &recovered {
                    result.sent.splice(0..0, recovered.sent.clone());
                    if recovered.decision == crate::model::DecisionStatus::Sent {
                        result.decision = crate::model::DecisionStatus::Sent;
                    }
                }
                apply::apply(s, result, opts.consume)
            });
            let (session, result) = match (outcome, recovered) {
                // Everything was already on GitHub: the recovery is the result.
                (Err(err), Some(recovered)) if sink::github::is_nothing_to_submit(&err) => {
                    let session = store.load(&session_id)?;
                    let mut recovered = recovered;
                    recovered.summary = apply::summary(&recovered);
                    (session, recovered)
                }
                (outcome, _) => outcome?,
            };
            // Removed only after the result was saved: a crash before this
            // leaves the record for the next submit to reconcile.
            let _ = std::fs::remove_file(&pending);
            let mut session = session;
            session.pending_submit = pending.exists();
            Response::with_session(result, &session)
        }
        other => Err(Error::invalid(format!("unknown sink: {other}")).hint("run `reviewkit sink list`")),
    }
}

fn repo_ref_of(session: &crate::model::Session) -> Result<RepoRef> {
    match (&session.repo.host, &session.repo.owner, &session.repo.name) {
        (Some(host), Some(owner), Some(name)) => Ok(RepoRef {
            host: host.clone(),
            owner: owner.clone(),
            name: name.clone(),
        }),
        _ => Err(Error::invalid("the github sink needs a pull request session")),
    }
}
