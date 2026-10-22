use clap::{Args, Subcommand};
use serde::Serialize;

use crate::context::Context;
use crate::envelope::Response;
use crate::error::{Error, Result};
use crate::ext::gh::{self, SystemGh};
use crate::ext::git::{Repository, SystemGit};
use crate::model::{SessionSummary, Source};
use crate::source::local::{self, LocalSpec};
use crate::source::pr::{self, PrSource};
use crate::store::Store;

#[derive(Debug, Subcommand)]
pub enum SessionCommand {
    /// Open (or resume) a session and compute its targets.
    Open(OpenArgs),
    /// Print a session as it is stored.
    Get {
        #[arg(long, value_name = "ID")]
        session: String,
    },
    /// List stored sessions, newest first.
    List,
    /// Re-run the source: recompute targets and follow comments whose lines moved.
    Refresh {
        #[arg(long, value_name = "ID")]
        session: String,
        /// Accepted for forward compatibility; the whole session is recomputed regardless.
        #[arg(long, value_name = "PATH")]
        path: Vec<String>,
    },
    /// Close a session. With --purge, delete its state.
    Close {
        #[arg(long, value_name = "ID")]
        session: String,
        #[arg(long)]
        purge: bool,
    },
}

#[derive(Debug, Args)]
#[command(group(clap::ArgGroup::new("what").required(true).multiple(false)))]
pub struct OpenArgs {
    /// Review the working tree (or --head) against --base in the repository at --cwd.
    #[arg(long, group = "what")]
    pub local: bool,
    /// Review a pull request by number (of the origin of --cwd) or URL.
    #[arg(long, group = "what", value_name = "NUMBER|URL")]
    pub pr: Option<String>,
    /// Resume an existing session by id.
    #[arg(long, group = "what", value_name = "ID")]
    pub session: Option<String>,
    /// Base revision for --local. Defaults to HEAD. `A...` / `A...B` use the
    /// merge-base, like `git diff A...B`.
    #[arg(long, value_name = "REV", requires = "local")]
    pub base: Option<String>,
    /// Head revision for --local. Defaults to the working tree.
    #[arg(long, value_name = "REV", requires = "local")]
    pub head: Option<String>,
}

pub fn run(ctx: &Context, command: SessionCommand) -> Result<Response> {
    let store = Store::new(ctx.state_dir.clone());
    match command {
        SessionCommand::Open(args) => open(ctx, &store, args),
        SessionCommand::Get { session } => {
            let session = store.load(&session)?;
            Response::with_session(serde_json::Value::Null, &session)
        }
        SessionCommand::Refresh { session, .. } => {
            let git = SystemGit::new(ctx.tools.git.as_deref())?;
            let session = refresh(ctx, &git, &store, &session)?;
            Response::with_session(serde_json::Value::Null, &session)
        }
        SessionCommand::List => {
            let summaries: Vec<SessionSummary> = store.list()?.iter().map(|s| s.summary()).collect();
            Response::new(summaries)
        }
        SessionCommand::Close { session, purge } => {
            let loaded = store.load(&session);
            if !purge {
                loaded?;
            } else {
                // A session this CLI cannot parse (written by a newer one) is
                // still removed; only its PR worktree is left for git to prune.
                // Under the fetch lock so a refresh in flight cannot recreate
                // the worktree after it was removed.
                store.with_fetch_lock(&session, || {
                    if let Ok(loaded) = &loaded
                        && matches!(loaded.source, Source::Pr { .. })
                    {
                        let git = SystemGit::new(ctx.tools.git.as_deref())?;
                        pr::purge(loaded, &git, store.state_dir());
                    }
                    store.remove(&session)
                })?;
            }
            Response::new(Closed { closed: true })
        }
    }
}

/// A pull request is fetched before the lock is taken and applied under it:
/// GitHub and `git fetch` can take seconds, and holding the lock meanwhile
/// would fail every comment and viewed toggle with E_LOCKED. Local sessions
/// only run local git and refresh under the lock.
pub fn refresh(ctx: &Context, git: &SystemGit, store: &Store, id: &str) -> Result<crate::model::Session> {
    refresh_inner(ctx, git, store, id).map_err(|err| crate::ext::git::name_session(err, id))
}

fn refresh_inner(ctx: &Context, git: &SystemGit, store: &Store, id: &str) -> Result<crate::model::Session> {
    let current = store.load(id)?;
    let (session, ()) = match &current.source {
        Source::Local { worktree, .. } => {
            let repo = Repository::at(git, worktree.clone());
            store.update_if_changed(id, |s| local::refresh(s, &repo))?
        }
        Source::Pr { .. } => {
            let gh = SystemGh::new(ctx.tools.gh.as_deref())?;
            let local_repo = Repository::at(git, current.repo.root.clone());
            let src = PrSource {
                git,
                gh: &gh,
                state: &ctx.state_dir,
                local_repo: Some(&local_repo),
            };
            store.with_fetch_lock(id, || {
                let current = store.load_for_write(id)?;
                let snapshot = pr::fetch(&current, &src, store, None)?;
                store.update_if_changed(id, |s| crate::core::refresh::apply(s, snapshot, git))
            })?
        }
    };
    Ok(session)
}

#[derive(Serialize)]
struct Closed {
    closed: bool,
}

fn open(ctx: &Context, store: &Store, args: OpenArgs) -> Result<Response> {
    let git = SystemGit::new(ctx.tools.git.as_deref())?;
    let session = if let Some(id) = args.session {
        refresh(ctx, &git, store, &id)?
    } else if let Some(arg) = args.pr {
        let gh = SystemGh::new(ctx.tools.gh.as_deref())?;
        let (number, repo_ref) = gh::parse_pr_arg(&arg)?;
        // A bare number needs the checkout to know which repository; a URL
        // does not, so it also works outside any git repository.
        let local_repo = match Repository::discover(&git, &ctx.cwd) {
            Ok(repo) => Some(repo),
            Err(err) if repo_ref.is_some() => {
                log::debug!("no checkout at --cwd: {}", err.message);
                None
            }
            Err(err) => return Err(err),
        };
        let from_remote = repo_ref.is_none();
        let repo_ref = match (repo_ref, &local_repo) {
            (Some(r), _) => r,
            (None, Some(local)) => pr::repo_ref_of(local)?,
            (None, None) => unreachable!("discover failed only with a URL"),
        };
        let src = PrSource {
            git: &git,
            gh: &gh,
            state: &ctx.state_dir,
            local_repo: local_repo.as_ref(),
        };
        pr::open(store, &src, &repo_ref, number).map_err(|err| {
            if from_remote {
                pr::hint_remote_host(err, &repo_ref.host)
            } else {
                err
            }
        })?
    } else {
        let repo = Repository::discover(&git, &ctx.cwd)?;
        let base = args.base.unwrap_or_else(|| "HEAD".to_string());
        let names_both = match base.split_once("...") {
            Some((_, right)) => !right.is_empty(),
            None => base.contains(".."),
        };
        if names_both && args.head.is_some() {
            return Err(Error::invalid(format!("{base} already names both sides; drop --head")));
        }
        let spec = LocalSpec::new(base, args.head);
        local::open(store, &repo, &spec)?
    };
    Response::with_session(serde_json::Value::Null, &session)
}
