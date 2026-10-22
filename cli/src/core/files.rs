use std::path::{Component, Path};

use crate::core::TOO_LARGE_BYTES;
use crate::core::comments::SideFile;
use crate::error::{Error, Result};
use crate::ext::git::{Git, Repository};
use crate::model::{Session, Side};

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
    let limit = TOO_LARGE_BYTES as u64;
    let large = |blob: Option<String>| SideFile {
        blob,
        binary: false,
        too_large: true,
        lines: Vec::new(),
    };
    let (bytes, blob) = match (side, session.head.sha.as_deref()) {
        (Side::Base, _) => {
            let git_path = base_git_path(session, path);
            if !repo.is_blob(&session.base.sha, git_path)? {
                return Ok(None);
            }
            let blob = target
                .and_then(|t| t.base_blob.clone())
                .or_else(|| repo.blob_id(&session.base.sha, git_path));
            if repo.blob_size(&session.base.sha, git_path).is_some_and(|s| s > limit) {
                return Ok(Some(large(blob)));
            }
            (repo.read_blob(&session.base.sha, git_path)?, blob)
        }
        (Side::Head, Some(sha)) => {
            if !repo.is_blob(sha, path)? {
                return Ok(None);
            }
            let blob = target
                .and_then(|t| t.head_blob.clone())
                .or_else(|| repo.blob_id(sha, path));
            if repo.blob_size(sha, path).is_some_and(|s| s > limit) {
                return Ok(Some(large(blob)));
            }
            (repo.read_blob(sha, path)?, blob)
        }
        (Side::Head, None) => {
            let worktree = session.source.worktree();
            // Otherwise a moved checkout reads as a file that does not exist.
            if !worktree.is_dir() {
                return Err(crate::ext::git::checkout_not_found(worktree));
            }
            let full = worktree.join(path);
            let meta = match std::fs::metadata(&full) {
                Ok(meta) if meta.is_file() => meta,
                _ => return Ok(None),
            };
            if meta.len() > limit {
                // Not the Target's head_blob: that is the file as of the last
                // refresh. Not `-w`: nothing diffs a file this large later.
                return Ok(Some(large(worktree_blob(repo, path, false)?)));
            }
            // Removed since `metadata` (a delete-then-write save, a checkout
            // mid-refresh): failing here would fail the whole refresh.
            let bytes = match std::fs::read(&full) {
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                other => other?,
            };
            (bytes, worktree_blob(repo, path, true)?)
        }
    };
    let binary = is_binary(&bytes);
    let lines = if binary {
        Vec::new()
    } else {
        split_lines(&String::from_utf8_lossy(&bytes))
    };
    Ok(Some(SideFile {
        blob,
        binary,
        too_large: false,
        lines,
    }))
}

/// The id only lets the comment follow the file later; a file git cannot
/// hash (a clean filter that cannot run, objects that cannot be written)
/// is still read, or one such file would fail every refresh of the session.
/// Only git refusing the file is tolerated: git that cannot start or times
/// out is reported, as everywhere else.
fn worktree_blob(repo: &Repository<'_>, path: &str, write: bool) -> Result<Option<String>> {
    let refused = |err: &Error| !err.details["exit"].is_null();
    let hashed = if write {
        // Without the object nothing can diff it later, but the id alone
        // still tells whether the file changed.
        match repo.hash_object_write(path) {
            Err(err) if refused(&err) => repo.hash_object(path),
            other => other,
        }
    } else {
        repo.hash_object(path)
    };
    match hashed {
        Ok(id) => Ok(Some(id)),
        Err(err) if refused(&err) => {
            log::warn!(
                "cannot hash {path}; its comments will not follow edits: {}",
                err.message
            );
            Ok(None)
        }
        Err(err) => Err(err),
    }
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

/// git's own heuristic: a NUL in the first 8000 bytes.
pub fn is_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(8000).any(|b| *b == 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct HashFails;

    impl Git for HashFails {
        fn run(&self, _cwd: &Path, _args: &[&str]) -> Result<Vec<u8>> {
            Err(Error::tool("git", crate::error::ErrorCode::EGit, "hash-object failed")
                .details(serde_json::json!({ "exit": 128 })))
        }
    }

    struct GitCannotStart;

    impl Git for GitCannotStart {
        fn run(&self, _cwd: &Path, _args: &[&str]) -> Result<Vec<u8>> {
            Err(Error::tool(
                "git",
                crate::error::ErrorCode::EToolMissing,
                "cannot run git",
            ))
        }
    }

    #[test]
    fn a_git_that_cannot_start_is_reported_not_read_as_a_file_without_a_blob() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("small.txt"), "a\n").unwrap();
        let session = worktree_session(dir.path());
        let repo = Repository::at(&GitCannotStart, dir.path().to_path_buf());
        let err = read_side(&session, &repo, Side::Head, "small.txt").err();
        assert_eq!(err.map(|e| e.code), Some(crate::error::ErrorCode::EToolMissing));
    }

    fn worktree_session(dir: &Path) -> Session {
        use crate::model::{HeadRef, Repo, RevRef, Source};
        Session::new(
            "local/x/y".into(),
            Repo {
                root: dir.to_path_buf(),
                host: None,
                owner: None,
                name: None,
                remote_url: None,
            },
            Source::Local {
                base_spec: "HEAD".into(),
                head_spec: "worktree".into(),
                worktree: dir.to_path_buf(),
            },
            RevRef {
                r#ref: Some("HEAD".into()),
                sha: "a".repeat(40),
            },
            HeadRef {
                r#ref: None,
                sha: None,
                dirty: true,
            },
            "t".into(),
        )
    }

    #[test]
    fn a_working_tree_file_git_cannot_hash_is_still_read_without_a_blob() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("small.txt"), "a\n").unwrap();
        std::fs::write(dir.path().join("large.txt"), vec![b'a'; TOO_LARGE_BYTES + 1]).unwrap();
        let session = worktree_session(dir.path());
        let repo = Repository::at(&HashFails, dir.path().to_path_buf());
        for path in ["small.txt", "large.txt"] {
            let side = read_side(&session, &repo, Side::Head, path).unwrap().unwrap();
            assert_eq!(side.blob, None, "{path}");
        }
        assert_eq!(
            read_side(&session, &repo, Side::Head, "small.txt")
                .unwrap()
                .unwrap()
                .lines,
            vec!["a"]
        );
    }

    #[test]
    fn a_nul_in_the_first_8000_bytes_means_binary() {
        assert!(is_binary(b"ab\0c"));
        assert!(!is_binary(b"plain"));
    }

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
