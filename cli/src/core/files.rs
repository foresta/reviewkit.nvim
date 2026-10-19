use std::path::{Component, Path};

use crate::core::comments::SideFile;
use crate::error::{Error, Result};
use crate::ext::git::{Git, Repository};
use crate::model::{Session, Side};
use crate::source::local::is_binary;

/// For a pull request `repo.root` is the reviewer's own checkout, which
/// never fetched the PR's commits; only the PR worktree (backed by the
/// mirror) has them. Every object read of a session goes through here.
pub fn object_repo<'g>(git: &'g dyn Git, session: &Session) -> Repository<'g> {
    Repository::at(git, session.source.worktree().to_path_buf())
}

/// Paths in the contract are always the head-side path of a Target. On the
/// base side a renamed file lives under `Target.old_path`; that mapping is
/// resolved here so callers never pass the old name.
pub fn base_git_path<'a>(session: &'a Session, path: &'a str) -> &'a str {
    session
        .targets
        .iter()
        .find(|t| t.path == path)
        .and_then(|t| t.old_path.as_deref())
        .unwrap_or(path)
}

/// Relative, normalised paths only: `..` or an absolute path would read
/// outside the worktree and never match a Target.
pub fn check_path(path: &str) -> Result<()> {
    let p = Path::new(path);
    let ok = !path.is_empty() && p.components().all(|c| matches!(c, Component::Normal(_))) && !path.ends_with('/');
    if ok {
        Ok(())
    } else {
        Err(Error::invalid(format!(
            "path must be relative to the repository root, without `..`: {path:?}"
        )))
    }
}

/// Reads one side of a path as the comment anchor sees it. Head is the
/// working tree when the session has no head commit, a blob otherwise.
pub fn read_side(session: &Session, repo: &Repository<'_>, side: Side, path: &str) -> Result<Option<SideFile>> {
    check_path(path)?;
    let target = session.targets.iter().find(|t| t.path == path);
    let (bytes, blob) = match (side, session.head.sha.as_deref()) {
        (Side::Base, _) => {
            let git_path = base_git_path(session, path);
            if !repo.is_blob(&session.base.sha, git_path) {
                return Ok(None);
            }
            let blob = target
                .and_then(|t| t.base_blob.clone())
                .or_else(|| repo.blob_id(&session.base.sha, git_path));
            (repo.read_blob(&session.base.sha, git_path)?, blob)
        }
        (Side::Head, Some(sha)) => {
            if !repo.is_blob(sha, path) {
                return Ok(None);
            }
            let blob = target
                .and_then(|t| t.head_blob.clone())
                .or_else(|| repo.blob_id(sha, path));
            (repo.read_blob(sha, path)?, blob)
        }
        (Side::Head, None) => {
            let full = session.source.worktree().join(path);
            if !full.is_file() {
                return Ok(None);
            }
            (std::fs::read(&full)?, repo.hash_object_write(path).ok())
        }
    };
    let binary = is_binary(&bytes);
    let lines = if binary {
        Vec::new()
    } else {
        split_lines(&String::from_utf8_lossy(&bytes))
    };
    Ok(Some(SideFile { blob, binary, lines }))
}

/// Editor line numbers: a trailing newline does not start a line, and CRLF
/// files are matched without the `\r` so context survives EOL conversion.
pub fn split_lines(text: &str) -> Vec<String> {
    let mut lines: Vec<String> = text
        .split('\n')
        .map(|l| l.strip_suffix('\r').unwrap_or(l).to_string())
        .collect();
    if text.ends_with('\n') || text.is_empty() {
        lines.pop();
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_lines_matches_editor_line_numbers() {
        assert_eq!(split_lines(""), Vec::<String>::new());
        assert_eq!(split_lines("a\n"), vec!["a"]);
        assert_eq!(split_lines("a\nb"), vec!["a", "b"]);
        assert_eq!(split_lines("a\n\nb\n"), vec!["a", "", "b"]);
        assert_eq!(split_lines("a\r\nb\r\n"), vec!["a", "b"]);
    }

    #[test]
    fn paths_must_stay_inside_the_repository() {
        assert!(check_path("src/a.rs").is_ok());
        assert!(check_path("dir/with space.txt").is_ok());
        for bad in ["", "/abs", "../x", "a/../b", "./a", "dir/"] {
            assert!(check_path(bad).is_err(), "{bad:?}");
        }
    }
}
