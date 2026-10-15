use std::collections::HashMap;

use crate::core::{TOO_LARGE_BYTES, commentable};
use crate::error::Result;
use crate::ext::diffparse::{self, Patch};
use crate::ext::git::{RawChange, Repository};
use crate::model::{CommentableRanges, Hunk, Stats, Target, TargetReason, TargetStatus};

/// Shared by every source: only how base and head are chosen differs
/// between a local diff and a pull request.
pub fn build(repo: &Repository<'_>, base_sha: &str, head: Option<&str>) -> Result<Vec<Target>> {
    let changes = repo.diff_raw(base_sha, head)?;
    let patches = patches_by_path(diffparse::parse(&repo.diff_patch(base_sha, head)?));
    let mut targets: Vec<Target> = changes
        .into_iter()
        .map(|change| {
            let patch = patches
                .get(&change.path)
                .cloned()
                .unwrap_or_else(|| empty_patch(&change.path));
            target_from_change(change, patch, repo, head)
        })
        .collect();
    if head.is_none() {
        for path in repo.untracked()? {
            match untracked_target(repo, &path.to_string_lossy()) {
                Ok(target) => push_untracked(&mut targets, target),
                // A file that vanished or cannot be read is not worth failing
                // the whole session for.
                Err(err) => log::warn!("skipping untracked {}: {err}", path.to_string_lossy()),
            }
        }
    }
    for (i, target) in targets.iter_mut().enumerate() {
        target.id = format!("t_{}", i + 1);
    }
    Ok(targets)
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

/// A path deleted in the index but recreated untracked would otherwise be
/// two Targets; it is really one modified file, so the untracked view wins
/// and keeps the deleted entry's base blob and base ranges.
fn push_untracked(targets: &mut Vec<Target>, mut untracked: Target) {
    if let Some(index) = targets.iter().position(|t| t.path == untracked.path) {
        let deleted = targets.remove(index);
        untracked.status = TargetStatus::Modified;
        untracked.base_blob = deleted.base_blob;
        untracked.commentable.base = deleted.commentable.base;
    }
    targets.push(untracked);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_counting_matches_git_for_missing_trailing_newline() {
        assert_eq!(count_lines(b""), 0);
        assert_eq!(count_lines(b"a\n"), 1);
        assert_eq!(count_lines(b"a\nb"), 2);
        assert!(is_binary(b"ab\0c"));
        assert!(!is_binary(b"plain"));
    }
}
