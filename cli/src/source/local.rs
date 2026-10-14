use std::collections::HashMap;
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::core::{TOO_LARGE_BYTES, commentable, time};
use crate::error::{Error, Result};
use crate::ext::diffparse::{self, Patch};
use crate::ext::git::{RawChange, Repository};
use crate::model::{
    CommentableRanges, HeadRef, Hunk, Repo, RevRef, Session, Source, Stats, Target, TargetReason, TargetStatus,
};
use crate::store::Store;

pub const WORKTREE: &str = "worktree";

pub struct LocalSpec {
    pub base: String,
    /// `None` compares the base against the working tree.
    pub head: Option<String>,
}

impl LocalSpec {
    fn head_label(&self) -> &str {
        self.head.as_deref().unwrap_or(WORKTREE)
    }
}

/// `local/<sha256(root)[:12]>/<base>..<head>`. The root hash keeps sessions
/// of different checkouts apart; specs are path-safe because `/` would split
/// the id into extra directories.
pub fn session_id(root: &Path, spec: &LocalSpec) -> String {
    let digest = Sha256::digest(root.to_string_lossy().as_bytes());
    let root_key: String = digest[..6].iter().map(|b| format!("{b:02x}")).collect();
    format!(
        "local/{root_key}/{}..{}",
        spec.base.replace('/', "__"),
        spec.head_label().replace('/', "__")
    )
}

/// Specs are resolved before the store touches disk, so a typo in --base
/// does not leave an empty session directory behind.
pub fn open(store: &Store, repo: &Repository<'_>, spec: &LocalSpec) -> Result<Session> {
    repo.resolve_commit(&spec.base)?;
    if let Some(head) = &spec.head {
        repo.resolve_commit(head)?;
    }
    let id = session_id(repo.root(), spec);
    let (session, ()) = store.open_or_create(
        &id,
        || Ok(new_session(id.clone(), repo, spec)),
        |session| refresh(session, repo),
    )?;
    Ok(session)
}

fn new_session(id: String, repo: &Repository<'_>, spec: &LocalSpec) -> Session {
    let remote_url = repo.remote_url("origin");
    let (host, owner, name) = remote_url.as_deref().and_then(parse_remote).unwrap_or_default();
    Session::new(
        id,
        Repo {
            root: repo.root().to_path_buf(),
            host,
            owner,
            name,
            remote_url,
        },
        Source::Local {
            base_spec: spec.base.clone(),
            head_spec: spec.head_label().to_string(),
            worktree: repo.root().to_path_buf(),
        },
        RevRef {
            r#ref: Some(spec.base.clone()),
            sha: String::new(),
        },
        HeadRef {
            r#ref: spec.head.clone(),
            sha: None,
            dirty: false,
        },
        time::now(),
    )
}

/// Re-runs the diff and rebuilds `targets`, `base` and `head`. Comments are
/// not re-anchored here; line shifting on refresh lives in core.
pub fn refresh(session: &mut Session, repo: &Repository<'_>) -> Result<()> {
    let Source::Local {
        base_spec, head_spec, ..
    } = &session.source
    else {
        return Err(Error::invalid("not a local session"));
    };
    let base_spec = base_spec.clone();
    let head_spec = (head_spec != WORKTREE).then(|| head_spec.clone());

    let base_sha = repo.resolve_commit(&base_spec)?;
    let head_sha = head_spec.as_deref().map(|spec| repo.resolve_commit(spec)).transpose()?;

    let changes = repo.diff_raw(&base_sha, head_sha.as_deref())?;
    let patches = patches_by_path(diffparse::parse(&repo.diff_patch(&base_sha, head_sha.as_deref())?));
    let mut targets: Vec<Target> = changes
        .into_iter()
        .map(|change| {
            let patch = patches
                .get(&change.path)
                .cloned()
                .unwrap_or_else(|| empty_patch(&change.path));
            target_from_change(change, patch, repo, head_sha.as_deref())
        })
        .collect();
    if head_sha.is_none() {
        for path in repo.untracked()? {
            match untracked_target(repo, &path.to_string_lossy()) {
                Ok(target) => targets.push(target),
                // A file that vanished or cannot be read is not worth failing
                // the whole session for.
                Err(err) => log::warn!("skipping untracked {}: {err}", path.to_string_lossy()),
            }
        }
    }
    for (i, target) in targets.iter_mut().enumerate() {
        target.id = format!("t_{}", i + 1);
        target.viewed = session
            .viewed
            .get(&target.path)
            .is_some_and(|v| Some(&v.blob) == target.head_blob.as_ref());
    }

    session.base = RevRef {
        r#ref: Some(base_spec),
        sha: base_sha,
    };
    session.head = HeadRef {
        r#ref: head_spec,
        dirty: head_sha.is_none() && repo.is_dirty()?,
        sha: head_sha,
    };
    session.targets = targets;
    Ok(())
}

/// A typechange (symlink → file) is printed as two blocks for one path;
/// their hunks belong to the same Target.
fn patches_by_path(patches: Vec<Patch>) -> HashMap<String, Patch> {
    let mut merged: HashMap<String, Patch> = HashMap::new();
    for patch in patches {
        match merged.get_mut(&patch.path) {
            Some(existing) => {
                existing.binary |= patch.binary;
                existing.stats.added += patch.stats.added;
                existing.stats.removed += patch.stats.removed;
                existing.hunks.extend(patch.hunks);
                existing.context_base.extend(patch.context_base);
                existing.context_head.extend(patch.context_head);
                existing.bytes += patch.bytes;
            }
            None => {
                merged.insert(patch.path.clone(), patch);
            }
        }
    }
    merged
}

fn empty_patch(path: &str) -> Patch {
    Patch {
        path: path.to_string(),
        binary: false,
        stats: Stats::default(),
        hunks: Vec::new(),
        context_base: Vec::new(),
        context_head: Vec::new(),
        bytes: 0,
    }
}

fn target_from_change(change: RawChange, patch: Patch, repo: &Repository<'_>, head: Option<&str>) -> Target {
    let status = match change.status {
        'A' => TargetStatus::Added,
        'D' => TargetStatus::Deleted,
        'R' => TargetStatus::Renamed,
        'C' => TargetStatus::Copied,
        'T' => TargetStatus::Type,
        'M' => TargetStatus::Modified,
        other => {
            log::warn!("{}: unexpected git status {other:?}, treating as modified", change.path);
            TargetStatus::Modified
        }
    };
    let too_large = patch.bytes > TOO_LARGE_BYTES;
    // Against the working tree git prints zeros for the head blob.
    let head_blob = match (change.head_blob, status, head) {
        (Some(blob), _, _) => Some(blob),
        (None, TargetStatus::Deleted, _) => None,
        (None, _, Some(head)) => repo.blob_id(head, &change.path),
        (None, _, None) => repo.hash_object(&change.path).ok(),
    };
    Target {
        id: String::new(),
        path: change.path,
        old_path: change.old_path,
        status,
        reason: TargetReason::Changed,
        base_blob: change.base_blob,
        head_blob,
        binary: patch.binary,
        too_large,
        viewed: false,
        stats: patch.stats,
        hunks: if too_large { Vec::new() } else { patch.hunks },
        commentable: CommentableRanges {
            base: commentable::merge(patch.context_base),
            head: commentable::merge(patch.context_head),
        },
        range: None,
    }
}

fn untracked_target(repo: &Repository<'_>, path: &str) -> Result<Target> {
    let bytes = std::fs::read(repo.root().join(path))?;
    let binary = is_binary(&bytes);
    let too_large = bytes.len() > TOO_LARGE_BYTES;
    let lines = if binary { 0 } else { count_lines(&bytes) };
    let whole_file = (lines > 0 && !too_large).then_some(Hunk {
        base_start: 0,
        base_count: 0,
        head_start: 1,
        head_count: lines,
    });
    Ok(Target {
        id: String::new(),
        path: path.to_string(),
        old_path: None,
        status: TargetStatus::Untracked,
        reason: TargetReason::Changed,
        base_blob: None,
        head_blob: repo.hash_object(path).ok(),
        binary,
        too_large,
        viewed: false,
        stats: Stats {
            added: lines,
            removed: 0,
        },
        hunks: whole_file.into_iter().collect(),
        commentable: CommentableRanges {
            base: Vec::new(),
            head: if lines > 0 { vec![[1, lines]] } else { Vec::new() },
        },
        range: None,
    })
}

/// git's own heuristic: a NUL in the first 8000 bytes.
pub fn is_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(8000).any(|b| *b == 0)
}

pub fn count_lines(bytes: &[u8]) -> u32 {
    if bytes.is_empty() {
        return 0;
    }
    let newlines = bytes.iter().filter(|b| **b == b'\n').count();
    (newlines + usize::from(!bytes.ends_with(b"\n"))) as u32
}

/// `https://host/owner/name(.git)` and `git@host:owner/name(.git)`. Anything
/// else is left unparsed rather than guessed.
fn parse_remote(url: &str) -> Option<(Option<String>, Option<String>, Option<String>)> {
    let rest = if let Some(rest) = url.strip_prefix("https://").or_else(|| url.strip_prefix("http://")) {
        rest.split_once('/')
            .map(|(host, path)| (host.split('@').next_back().unwrap_or(host), path))
    } else if let Some(rest) = url.strip_prefix("ssh://") {
        let rest = rest.split_once('@').map(|(_, r)| r).unwrap_or(rest);
        rest.split_once('/')
    } else {
        url.split_once('@').and_then(|(_, r)| r.split_once(':'))
    }?;
    let (host, path) = rest;
    let path = path.trim_end_matches('/').trim_end_matches(".git");
    let (owner, name) = path.rsplit_once('/')?;
    Some((Some(host.to_string()), Some(owner.to_string()), Some(name.to_string())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_id_is_stable_and_path_safe() {
        let spec = LocalSpec {
            base: "origin/main".into(),
            head: None,
        };
        let id = session_id(Path::new("/repo"), &spec);
        assert!(id.starts_with("local/"));
        assert!(id.ends_with("/origin__main..worktree"));
        assert_eq!(id, session_id(Path::new("/repo"), &spec));
    }

    #[test]
    fn remote_urls_parse_to_host_owner_name() {
        let expect = Some((
            Some("github.com".into()),
            Some("foresta".into()),
            Some("reviewkit.nvim".into()),
        ));
        assert_eq!(parse_remote("https://github.com/foresta/reviewkit.nvim.git"), expect);
        assert_eq!(parse_remote("git@github.com:foresta/reviewkit.nvim.git"), expect);
        assert_eq!(parse_remote("ssh://git@github.com/foresta/reviewkit.nvim"), expect);
        assert_eq!(parse_remote("/local/path"), None);
    }

    #[test]
    fn line_counting_matches_git_for_missing_trailing_newline() {
        assert_eq!(count_lines(b""), 0);
        assert_eq!(count_lines(b"a\n"), 1);
        assert_eq!(count_lines(b"a\nb"), 2);
        assert!(is_binary(b"ab\0c"));
        assert!(!is_binary(b"plain"));
    }
}
