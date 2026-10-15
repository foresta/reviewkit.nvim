use std::path::Path;

use sha2::{Digest, Sha256};

use crate::core::{anchor, time, viewed};
use crate::error::{Error, Result};
use crate::ext::git::{Repository, parse_remote_url};
use crate::model::{HeadRef, Repo, RevRef, Session, Source};
use crate::source::targets;
use crate::store::Store;

pub use crate::source::targets::{count_lines, is_binary};

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

/// Re-runs the diff, rebuilds `targets`, `base` and `head`, then follows
/// every comment across whatever changed on its side.
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
    let targets = targets::build(repo, &base_sha, head_sha.as_deref())?;

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
    viewed::refresh_flags(session);
    anchor::reanchor(session, repo)
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
}
