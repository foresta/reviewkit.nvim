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

/// gh cannot tell an SSH config alias (`Host work`, `Host github.com-work`)
/// from a real host, and neither can reviewkit: a dotless name is often a
/// GitHub Enterprise host. So the host is tried as is, and only a gh failure
/// to reach or log into it points at the alias; an answer from the host
/// (a missing PR, a rejected token) says the host is real.
pub fn hint_remote_host(err: Error, host: &str) -> Error {
    let from_gh = err.tool.as_deref() == Some("gh") && matches!(err.code, ErrorCode::EAuth | ErrorCode::ETool);
    if !from_gh || host == "github.com" || !names_an_unreachable_host(&err.message) {
        return err;
    }
    let alias = "the remote's host may be an SSH alias; pass the pull request URL instead";
    let hint = match &err.hint {
        Some(hint) => format!("{hint}; or {alias}"),
        None => alias.to_string(),
    };
    err.hint(hint)
}

/// gh's wording; the message carries its stderr.
fn names_an_unreachable_host(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    [
        "error connecting to",
        "could not resolve host",
        "no such host",
        "connection refused",
        "dial tcp",
        "not logged into",
        "unknown host",
        "none of the git remotes",
    ]
    .iter()
    .any(|needle| message.contains(needle))
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
    // Fetched under the fetch lock only, as `session refresh` does: the
    // session lock is held just to apply what was fetched, so other commands
    // are not failed with E_LOCKED while GitHub and git are slow.
    let (session, ()) = store.with_new_fetch_lock(&id, || {
        // A reopen may have waited for another fetch; the PR is read again
        // so what that fetch saw is not applied stale.
        let meta = if store.exists(&id) {
            store.load_for_write(&id)?;
            gh::pr_meta(src.gh, repo_ref, number)?
        } else {
            meta
        };
        let draft = new_session(id.clone(), src, repo_ref, &meta);
        let snapshot = fetch(&draft, src, store, Some(meta))?;
        store.open_or_create(&id, || Ok(draft), |s| refresh::apply(s, snapshot, src.git))
    })?;
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
    let details = gh::pr_details(src.gh, &repo_ref, number, time::now())?.without_markers();

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
/// no network) only from a complete checkout of the same repository; any
/// other would put another project's objects and remote behind this PR's
/// id, or history and blobs the mirror cannot fetch on its own. The PR
/// head is fetched only when it or the base moved.
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
    // Left by an interrupted older build (rename cannot replace it), or
    // seeded by an older build from a shallow or partial checkout, which
    // fails merge-base or reads for good.
    if !mirror_dir.join("HEAD").exists() || incomplete_mirror(&mirror_dir) {
        let _ = std::fs::remove_dir_all(&mirror_dir);
    }
    let seed = || local.filter(|r| !r.is_shallow() && !r.is_partial());
    let mirror = if mirror_dir.join("HEAD").exists() {
        Repository::at(src.git, mirror_dir)
    } else if let Some(seed) = seed() {
        match Repository::clone_bare(src.git, seed.root(), &mirror_dir, &remote) {
            Ok(mirror) => mirror,
            Err(err) => {
                log::warn!(
                    "seeding the mirror from {} failed, fetching instead: {}",
                    seed.root().display(),
                    err.message
                );
                Repository::init_bare(src.git, &mirror_dir, &remote)?
            }
        }
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
        // Only the URL reviewkit made up gets gh's credentials; a remote the
        // user configured keeps the user's own setup.
        let synthesized = format!("https://{}/{}/{}.git", repo_ref.host, repo_ref.owner, repo_ref.name);
        let config = match src.gh.executable() {
            Some(gh) if mirror.remote_url("origin").as_deref() == Some(synthesized.as_str()) => {
                crate::ext::git::gh_credential_config(&repo_ref.host, gh)
            }
            _ => vec![],
        };
        let fetch = |base_source: &str| {
            mirror.fetch_with(
                &config,
                "origin",
                &[
                    format!("+refs/pull/{}/head:{pr_ref}", meta.number),
                    format!("+{base_source}:{base_ref}"),
                ],
            )
        };
        let branch = format!("refs/heads/{}", meta.base_ref);
        // GitHub's recorded base commit first: a merged PR's branch is often
        // deleted (stacked PRs), and an open PR's may be deleted before
        // GitHub retargets it. The commit may in turn be gone after a
        // force-push; only then is the branch worth a second fetch (a network
        // failure would just fail twice).
        let fetched = match fetch(&meta.base_oid) {
            Err(err) if missing_on_remote(&err.message) => {
                log::warn!(
                    "fetching base {} failed, using {branch}: {}",
                    meta.base_oid,
                    err.message
                );
                fetch(&branch)
            }
            other => other,
        };
        fetched.map_err(|err| {
            let message = format!("cannot fetch pull request #{}: {}", meta.number, err.message);
            if err.code == ErrorCode::EAuth {
                Error::tool("git", ErrorCode::EAuth, message)
                    .hint(format!("run `gh auth login --hostname {}`", repo_ref.host))
            } else {
                Error::tool("git", ErrorCode::EGit, message)
            }
        })?;
    }
    let head = mirror
        .rev_parse(&pr_ref)
        .ok_or_else(|| Error::tool("git", ErrorCode::EGit, "the pull request head is missing after fetch"))?;
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

fn missing_on_remote(message: &str) -> bool {
    [
        "not our ref",
        "unadvertised object",
        "couldn't find remote ref",
        "no such remote ref",
    ]
    .iter()
    .any(|needle| message.contains(needle))
}

fn incomplete_mirror(dir: &std::path::Path) -> bool {
    dir.join("shallow").exists()
        || std::fs::read_dir(dir.join("objects/pack")).is_ok_and(|entries| {
            entries
                .filter_map(|e| e.ok())
                .any(|e| e.path().extension().is_some_and(|x| x == "promisor"))
        })
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
    // Read by `pr show` from any instance without the fetch lock: a torn
    // write would read as corrupt.
    store::write_atomically(path, &bytes)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rejected_token_on_a_real_host_gets_no_alias_hint() {
        let err = Error::tool(
            "gh",
            ErrorCode::EAuth,
            "gh api graphql failed: HTTP 401: Bad credentials (https://ghe/api/graphql)\nTry authenticating with:  gh auth login",
        );
        assert_eq!(super::hint_remote_host(err, "ghe").hint, None);
        let unreachable = Error::tool("gh", ErrorCode::ETool, "error connecting to ghe");
        assert!(super::hint_remote_host(unreachable, "ghe").hint.is_some());
    }

    #[test]
    fn only_a_missing_object_or_ref_sends_the_base_fetch_to_the_branch() {
        for gone in [
            "error: Server does not allow request for unadvertised object 1111",
            "fatal: remote error: upload-pack: not our ref 1111",
            "fatal: couldn't find remote ref refs/heads/main",
        ] {
            assert!(missing_on_remote(gone), "{gone}");
        }
        for failed in [
            "fatal: unable to access 'https://github.com/o/r.git/': Could not resolve host: github.com",
            "fatal: Could not read from remote repository.",
        ] {
            assert!(!missing_on_remote(failed), "{failed}");
        }
    }
}
