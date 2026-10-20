use crate::core::{anchor, threads, viewed};
use crate::error::Result;
use crate::ext::git::{Git, Repository};
use crate::model::{HeadRef, RevRef, Session, Source, Target, Thread};

/// What a source learned about the review, as values: sources never touch
/// the Session, so the follow-up every refresh needs lives in `apply` alone.
pub struct SourceSnapshot {
    pub source: Source,
    pub base: RevRef,
    pub head: HeadRef,
    pub targets: Vec<Target>,
    pub threads: Vec<Thread>,
}

/// Runs under the lock on the freshly read session, so comments added while
/// the snapshot was taken are re-anchored too.
pub fn apply(session: &mut Session, snapshot: SourceSnapshot, git: &dyn Git) -> Result<()> {
    let worktree = Repository::at(git, snapshot.source.worktree().to_path_buf());
    session.source = snapshot.source;
    session.base = snapshot.base;
    session.head = snapshot.head;
    session.targets = snapshot.targets;
    session.threads = snapshot.threads;
    threads::prune_actions(session);
    viewed::refresh_flags(session);
    anchor::reanchor(session, &worktree)
}
