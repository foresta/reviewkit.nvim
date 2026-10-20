use std::path::Path;

use sha2::{Digest, Sha256};

use crate::core::refresh::{self, SourceSnapshot};
use crate::core::time;
use crate::error::{Error, Result};
use crate::ext::git::{Repository, parse_remote_url};
use crate::model::{HeadRef, Repo, RevRef, Session, Source};
use crate::source::targets;
use crate::store::Store;

pub use crate::source::targets::count_lines;

pub const WORKTREE: &str = "worktree";

pub struct LocalSpec {
    pub base: String,
    /// `None` compares the base against the working tree.
    pub head: Option<String>,
}

impl LocalSpec {
    /// The base keeps the `A...` form rather than the resolved sha, so every
    /// refresh finds the merge-base again after either side moves.
    pub fn new(base: String, head: Option<String>) -> Self {
        match (base.split_once("..."), head) {
            (Some((a, b)), None) if !b.is_empty() => LocalSpec {
                base: format!("{}...", if a.is_empty() { "HEAD" } else { a }),
                head: Some(b.to_string()),
            },
            (_, head) => LocalSpec { base, head },
        }
    }

    fn head_label(&self) -> &str {
        self.head.as_deref().unwrap_or(WORKTREE)
    }
}

fn resolve_base(repo: &Repository<'_>, base: &str, head: Option<&str>) -> Result<String> {
    let Some(a) = base.strip_suffix("...") else {
        return repo.resolve_commit(base);
    };
    let other_spec = head.unwrap_or("HEAD");
    let a = repo.resolve_commit(a)?;
    let other = repo.resolve_commit(other_spec)?;
    repo.merge_base(&a, &other).map_err(|err| {
        // `git merge-base` exits 1 only when the histories share no commit.
        match err.details.get("exit").and_then(|e| e.as_i64()) {
            Some(1) => Error::invalid(format!("{base} has no merge-base with {other_spec}")),
            _ => err,
        }
    })
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
    if let Some(head) = &spec.head {
        repo.resolve_commit(head)?;
    }
    resolve_base(repo, &spec.base, spec.head.as_deref())?;
    let id = session_id(repo.root(), spec);
    let (session, ()) = store.open_or_create(
        &id,
        || Ok(new_session(id.clone(), repo, spec)),
        |session| refresh(session, repo),
    )?;
    Ok(session)
}

pub fn repo_of(repo: &Repository<'_>) -> Repo {
    let remote_url = repo.remote_url("origin");
    let (host, owner, name) = remote_url.as_deref().and_then(parse_remote_url).unwrap_or_default();
    Repo {
        root: repo.root().to_path_buf(),
        host,
        owner,
        name,
        remote_url,
    }
}

fn new_session(id: String, repo: &Repository<'_>, spec: &LocalSpec) -> Session {
    Session::new(
        id,
        repo_of(repo),
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

pub fn refresh(session: &mut Session, repo: &Repository<'_>) -> Result<()> {
    let snapshot = fetch(session, repo)?;
    refresh::apply(session, snapshot, repo.git())
}

/// Re-runs the diff for the session's specs.
pub fn fetch(session: &Session, repo: &Repository<'_>) -> Result<SourceSnapshot> {
    let Source::Local {
        base_spec, head_spec, ..
    } = &session.source
    else {
        return Err(Error::invalid("not a local session"));
    };
    let base_spec = base_spec.clone();
    let head_spec = (head_spec != WORKTREE).then(|| head_spec.clone());

    let base_sha = resolve_base(repo, &base_spec, head_spec.as_deref())?;
    let head_sha = head_spec.as_deref().map(|spec| repo.resolve_commit(spec)).transpose()?;
    let targets = targets::build(repo, &base_sha, head_sha.as_deref())?;

    Ok(SourceSnapshot {
        source: session.source.clone(),
        base: RevRef {
            r#ref: Some(base_spec),
            sha: base_sha,
        },
        head: HeadRef {
            r#ref: head_spec,
            dirty: head_sha.is_none() && repo.is_dirty()?,
            sha: head_sha,
        },
        targets,
        threads: vec![],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_three_dot_range_moves_its_right_side_to_the_head() {
        let spec = LocalSpec::new("main...feature".into(), None);
        assert_eq!((spec.base.as_str(), spec.head.as_deref()), ("main...", Some("feature")));
        let left_out = LocalSpec::new("...feature".into(), None);
        assert_eq!(left_out.base, "HEAD...");
        let kept = LocalSpec::new("main...".into(), None);
        assert_eq!((kept.base.as_str(), kept.head.as_deref()), ("main...", None));
        let explicit = LocalSpec::new("main...x".into(), Some("y".into()));
        assert_eq!(explicit.base, "main...x");
    }

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
}
