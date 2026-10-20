use std::path::Path;

use crate::core::refresh::{self, SourceSnapshot};
use crate::core::time;
use crate::error::{Error, ErrorCode, Result};
use crate::ext::gh::{self, Gh, PrMeta, RepoRef};
use crate::ext::git::{Git, Repository, parse_remote_url};
use crate::model::{
    CommentableRanges, HeadRef, PrInfo, PrState, Range, Repo, RevRef, Session, Source, Stats, Target, TargetReason,
    TargetStatus, Thread,
};
use crate::source::targets;
use crate::store::{self, StateDir, Store};

pub fn session_id(repo: &RepoRef, number: u64) -> String {
    format!("pr/{}/{}/{}/{}", repo.host, repo.owner, repo.name, number)
}

/// The repository a `--pr <number>` refers to: the origin of the checkout
/// at --cwd. A URL argument overrides it (`parse_pr_arg`).
pub fn repo_ref_of(repo: &Repository<'_>) -> Result<RepoRef> {
    let url = repo.remote_url("origin").ok_or_else(|| {
        Error::invalid("the repository has no `origin` remote").hint("pass the pull request URL instead")
    })?;
    let (host, owner, name) = parse_remote_url(&url).ok_or_else(|| {
        Error::invalid(format!("cannot tell the GitHub repository from {url}"))
            .hint("pass the pull request URL instead")
    })?;
    Ok(RepoRef {
        host: host.unwrap_or_default(),
        owner: owner.unwrap_or_default(),
        name: name.unwrap_or_default(),
    })
}

pub struct PrSource<'a> {
    pub git: &'a dyn Git,
    pub gh: &'a dyn Gh,
    pub state: &'a StateDir,
    /// The checkout at --cwd, if there is one. It seeds the mirror without
    /// network when its origin is the same repository; a URL can also be
    /// opened from outside any checkout.
    pub local_repo: Option<&'a Repository<'a>>,
}

/// GitHub owner and repository names are case-insensitive, but the state
/// directory may not be; the repository is re-read from the URL gh
/// reports so one PR never gets two sessions, mirrors or worktrees.
fn canonical(repo_ref: &RepoRef, meta: &PrMeta) -> RepoRef {
    match gh::parse_pr_arg(&meta.url) {
        Ok((_, Some(from_url))) => from_url,
        _ => repo_ref.clone(),
    }
}

pub fn open(store: &Store, src: &PrSource<'_>, repo_ref: &RepoRef, number: u64) -> Result<Session> {
    let meta = gh::pr_meta(src.gh, repo_ref, number)?;
    let repo_ref = &canonical(repo_ref, &meta);
    let id = session_id(repo_ref, number);
    let create = |meta: &PrMeta| {
        store.open_or_create(
            &id,
            || Ok(new_session(id.clone(), src, repo_ref, meta)),
            |session| refresh_with(session, src, store, Some(meta.clone())),
        )
    };
    // Reopening shares the PR worktree with any refresh that is running; the
    // PR is read again once that refresh is done, so it is not applied stale.
    let (session, ()) = if store.exists(&id) {
        store.with_fetch_lock(&id, || create(&gh::pr_meta(src.gh, repo_ref, number)?))?
    } else {
        create(&meta)?
    };
    Ok(session)
}

/// The checkout at --cwd only counts as "this repository" when its origin
/// names the same host/owner/name; otherwise `repo.root` is the PR's own
/// worktree, so cwd matching (herdr) and path mapping still work.
fn new_session(id: String, src: &PrSource<'_>, repo_ref: &RepoRef, meta: &PrMeta) -> Session {
    let worktree = src
        .state
        .pr_worktree(&repo_ref.host, &repo_ref.owner, &repo_ref.name, meta.number);
    let local = src.local_repo.filter(|r| same_repository(r, repo_ref));
    Session::new(
        id,
        Repo {
            root: local
                .map(|r| r.root().to_path_buf())
                .unwrap_or_else(|| worktree.clone()),
            host: Some(repo_ref.host.clone()),
            owner: Some(repo_ref.owner.clone()),
            name: Some(repo_ref.name.clone()),
            remote_url: Some(remote_url_of(local, repo_ref)),
        },
        Source::Pr {
            number: meta.number,
            url: meta.url.clone(),
            title: meta.title.clone(),
            author: meta.author.clone(),
            state: meta.state,
            head_ref: meta.head_ref.clone(),
            base_ref: meta.base_ref.clone(),
            base_oid: meta.base_oid.clone(),
            node_id: meta.node_id.clone(),
            worktree,
        },
        RevRef {
            r#ref: Some(meta.base_ref.clone()),
            sha: String::new(),
        },
        HeadRef {
            r#ref: Some(meta.head_ref.clone()),
            sha: None,
            dirty: false,
        },
        time::now(),
    )
}

pub fn refresh(session: &mut Session, src: &PrSource<'_>, store: &Store) -> Result<()> {
    refresh_with(session, src, store, None)
}

fn refresh_with(session: &mut Session, src: &PrSource<'_>, store: &Store, meta: Option<PrMeta>) -> Result<()> {
    let snapshot = fetch(session, src, store, meta)?;
    refresh::apply(session, snapshot, src.git)
}

/// Gathered without the session lock so other commands are not blocked on
/// the network. `pr.json` is written here, next to the session, so the
/// conversation and checks never travel in the envelope.
pub fn fetch(session: &Session, src: &PrSource<'_>, store: &Store, meta: Option<PrMeta>) -> Result<SourceSnapshot> {
    let (repo_ref, number) = match (&session.repo, &session.source) {
        (
            Repo {
                host: Some(host),
                owner: Some(owner),
                name: Some(name),
                ..
            },
            Source::Pr { number, .. },
        ) => (
            RepoRef {
                host: host.clone(),
                owner: owner.clone(),
                name: name.clone(),
            },
            *number,
        ),
        _ => return Err(Error::invalid("not a pull request session")),
    };
    let meta = match meta {
        Some(meta) => meta,
        None => gh::pr_meta(src.gh, &repo_ref, number)?,
    };
    let checkout = materialize(src, &repo_ref, &meta)?;
    let worktree = Repository::at(src.git, checkout.worktree.clone());
    let details = gh::pr_details(src.gh, &repo_ref, number, time::now())?;

    let mut targets = targets::build(&worktree, &checkout.base, Some(&checkout.head))?;
    add_thread_targets(&mut targets, &details.threads, &worktree, &checkout.head);

    let pr_path = store.state_dir().pr_file(&session.id)?;
    write_pr_info(&pr_path, &details.info)?;
    Ok(SourceSnapshot {
        source: Source::Pr {
            number: meta.number,
            url: meta.url,
            title: meta.title,
            author: meta.author,
            state: meta.state,
            head_ref: meta.head_ref.clone(),
            base_ref: meta.base_ref.clone(),
            base_oid: meta.base_oid,
            node_id: meta.node_id,
            worktree: checkout.worktree,
        },
        base: RevRef {
            r#ref: Some(meta.base_ref),
            sha: checkout.base,
        },
        head: HeadRef {
            r#ref: Some(meta.head_ref),
            sha: Some(checkout.head),
            dirty: false,
        },
        targets,
        threads: details.threads,
    })
}

struct Checkout {
    worktree: std::path::PathBuf,
    head: String,
    base: String,
}

/// Whether a checkout's origin is the pull request's repository, compared
/// case-insensitively because GitHub owner and repository names are.
fn same_repository(repo: &Repository<'_>, repo_ref: &RepoRef) -> bool {
    repo.remote_url("origin")
        .as_deref()
        .and_then(parse_remote_url)
        .is_some_and(|(host, owner, name)| {
            host.as_deref().is_some_and(|h| h.eq_ignore_ascii_case(&repo_ref.host))
                && owner
                    .as_deref()
                    .is_some_and(|o| o.eq_ignore_ascii_case(&repo_ref.owner))
                && name.as_deref().is_some_and(|n| n.eq_ignore_ascii_case(&repo_ref.name))
        })
}

fn remote_url_of(local: Option<&Repository<'_>>, repo_ref: &RepoRef) -> String {
    local
        .and_then(|r| r.remote_url("origin"))
        .unwrap_or_else(|| format!("https://{}/{}/{}.git", repo_ref.host, repo_ref.owner, repo_ref.name))
}

/// The mirror and its worktrees are shared by every PR of the repository,
/// while session locks only cover one PR; two PRs opened at once would
/// otherwise create, fetch into and prune the same repository together.
fn with_mirror_lock<T>(mirror_dir: &Path, f: impl FnOnce() -> Result<T>) -> Result<T> {
    let parent = mirror_dir
        .parent()
        .ok_or_else(|| Error::internal("mirror path has no parent"))?;
    std::fs::create_dir_all(parent)?;
    store::lock_while(
        &parent.join("mirror.lock"),
        store::FETCH_LOCK_TIMEOUT,
        "the repository mirror",
        f,
    )
}

/// Mirror + worktree under the state directory (architecture D2): the
/// user's checkout is only read. It seeds the mirror (hard-linked objects,
/// no network) only when it is a checkout of the same repository; anything
/// else would put another project's objects and remote behind this PR's
/// id. The PR head is fetched only when it or the base moved.
fn materialize(src: &PrSource<'_>, repo_ref: &RepoRef, meta: &PrMeta) -> Result<Checkout> {
    let mirror_dir = src.state.mirror_dir(&repo_ref.host, &repo_ref.owner, &repo_ref.name);
    with_mirror_lock(&mirror_dir, || {
        materialize_locked(src, repo_ref, meta, mirror_dir.clone())
    })
}

fn materialize_locked(
    src: &PrSource<'_>,
    repo_ref: &RepoRef,
    meta: &PrMeta,
    mirror_dir: std::path::PathBuf,
) -> Result<Checkout> {
    let local = src.local_repo.filter(|r| same_repository(r, repo_ref));
    let remote = remote_url_of(local, repo_ref);
    if !mirror_dir.join("HEAD").exists() {
        // Left by an interrupted older build; rename cannot replace it.
        let _ = std::fs::remove_dir_all(&mirror_dir);
    }
    let mirror = if mirror_dir.join("HEAD").exists() {
        Repository::at(src.git, mirror_dir)
    } else if let Some(local) = local {
        Repository::clone_bare(src.git, local.root(), &mirror_dir, &remote)?
    } else {
        Repository::init_bare(src.git, &mirror_dir, &remote)?
    };
    let pr_ref = format!("refs/reviewkit/pr/{}", meta.number);
    let base_ref = format!("refs/reviewkit/base/{}", meta.number);
    let have = mirror.rev_parse(&pr_ref);
    // An open PR's base ref must be GitHub's base oid, not merely present: a
    // retargeted PR (a stacked PR moved onto main) names another branch whose
    // tip the mirror may already hold. A closed PR keeps its recorded base
    // while the branch moves on, so it only needs that commit.
    let base_stale = if meta.state == PrState::Open {
        mirror.rev_parse(&base_ref).as_deref() != Some(meta.base_oid.as_str())
    } else {
        mirror.rev_parse(&base_ref).is_none() || !mirror.has_commit(&meta.base_oid)
    };
    if have.as_deref() != Some(meta.head_oid.as_str()) || base_stale {
        mirror
            .fetch(
                "origin",
                &[
                    format!("+refs/pull/{}/head:{pr_ref}", meta.number),
                    format!("+refs/heads/{}:{base_ref}", meta.base_ref),
                ],
            )
            .map_err(|err| {
                Error::new(
                    ErrorCode::EGit,
                    format!("cannot fetch pull request #{}: {}", meta.number, err.message),
                )
            })?;
    }
    let head = mirror
        .rev_parse(&pr_ref)
        .ok_or_else(|| Error::new(ErrorCode::EGit, "the pull request head is missing after fetch"))?;
    // A merged PR is contained in its base branch, so merge-base with the
    // branch would be the PR head itself; GitHub's recorded base commit
    // keeps the diff visible.
    let target = if meta.state != PrState::Open && mirror.has_commit(&meta.base_oid) {
        meta.base_oid.clone()
    } else {
        base_ref.clone()
    };
    let base = mirror.merge_base(&head, &target)?;
    let worktree = src
        .state
        .pr_worktree(&repo_ref.host, &repo_ref.owner, &repo_ref.name, meta.number);
    mirror.worktree_checkout(&worktree, &head)?;
    Ok(Checkout { worktree, head, base })
}

/// A thread on a file outside the diff still needs a Target, so the view
/// can open the file at the thread. Its status is `modified` only because
/// the enum has no "unchanged"; `reason` is what distinguishes it. Threads
/// whose file no longer exists at the head (outdated, deleted) get none:
/// there is nothing to open.
fn add_thread_targets(targets: &mut Vec<Target>, threads: &[Thread], worktree: &Repository<'_>, head: &str) {
    for thread in threads {
        if targets.iter().any(|t| t.path == thread.path) {
            continue;
        }
        let Some(line) = thread.line else { continue };
        let Some(head_blob) = worktree.blob_id(head, &thread.path) else {
            continue;
        };
        let start = thread.start_line.unwrap_or(line);
        targets.push(Target {
            id: format!("t_{}", targets.len() + 1),
            path: thread.path.clone(),
            old_path: None,
            status: TargetStatus::Modified,
            reason: TargetReason::Thread,
            base_blob: Some(head_blob.clone()),
            head_blob: Some(head_blob),
            binary: false,
            too_large: false,
            viewed: false,
            stats: Stats::default(),
            hunks: Vec::new(),
            commentable: CommentableRanges::default(),
            range: Some(Range { start, end: line }),
        });
    }
}

fn write_pr_info(path: &Path, info: &PrInfo) -> Result<()> {
    let bytes =
        serde_json::to_vec_pretty(info).map_err(|err| Error::internal(format!("cannot serialize pr.json: {err}")))?;
    std::fs::write(path, bytes)?;
    Ok(())
}

pub fn read_pr_info(path: &Path) -> Result<Option<PrInfo>> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|err| Error::internal(format!("cannot read {}: {err}", path.display()))),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err.into()),
    }
}

/// `session close --purge` for a PR: the worktree goes, the mirror stays
/// (it is shared by every PR of the repository and cheap to keep).
pub fn purge(session: &Session, git: &dyn Git, state: &StateDir) {
    if let (Some(host), Some(owner), Some(name), Source::Pr { worktree, .. }) = (
        &session.repo.host,
        &session.repo.owner,
        &session.repo.name,
        &session.source,
    ) {
        let mirror_dir = state.mirror_dir(host, owner, name);
        if !mirror_dir.exists() {
            let _ = std::fs::remove_dir_all(worktree);
            return;
        }
        let _ = with_mirror_lock(&mirror_dir, || {
            if mirror_dir.exists() {
                Repository::at(git, mirror_dir.clone()).worktree_remove(worktree);
            } else {
                let _ = std::fs::remove_dir_all(worktree);
            }
            Ok(())
        });
    }
}
