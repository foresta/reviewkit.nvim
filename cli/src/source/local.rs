use std::path::Path;

use sha2::{Digest, Sha256};

use crate::core::refresh::{self, SourceSnapshot};
use crate::core::time;
use crate::error::{Error, Result};
use crate::ext::git::{Repository, parse_remote_url};
use crate::model::{HeadRef, Repo, RevRef, Session, Source};
use crate::source::targets;
use crate::store::Store;

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
            // `A..B` is how git names the same comparison as `<base> <head>`;
            // an empty side is HEAD, as in git.
            (None, None) if base.contains("..") => {
                let (a, b) = base.split_once("..").expect("checked above");
                let or_head = |s: &str| {
                    if s.is_empty() {
                        "HEAD".to_string()
                    } else {
                        s.to_string()
                    }
                };
                LocalSpec {
                    base: or_head(a),
                    head: Some(or_head(b)),
                }
            }
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

/// `local/<sha256(root)[:12]>/<sha256(base, head)[:16]>`. The specs are
/// hashed rather than escaped: an escape that maps `/` to something else
/// collides with a spec that already contains it, and refs differing only in
/// case share a directory on case-insensitive file systems. The specs stay
/// readable in `Source::Local`.
pub fn session_id(root: &Path, spec: &LocalSpec) -> String {
    let hex = |bytes: &[u8]| -> String { bytes.iter().map(|b| format!("{b:02x}")).collect() };
    let root_key = hex(&Sha256::digest(root.to_string_lossy().as_bytes())[..6]);
    let mut specs = Sha256::new();
    specs.update(spec.base.as_bytes());
    specs.update([0]);
    specs.update(spec.head_label().as_bytes());
    format!("local/{root_key}/{}", hex(&specs.finalize()[..8]))
}

/// A hash collision must not resume another review in place of this one.
fn same_spec(session: &Session, spec: &LocalSpec) -> Result<()> {
    match &session.source {
        Source::Local {
            base_spec, head_spec, ..
        } if *base_spec == spec.base && head_spec == spec.head_label() => Ok(()),
        _ => Err(Error::new(
            crate::error::ErrorCode::EConflict,
            format!("session {} belongs to another comparison", session.id),
        )),
    }
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
        |session| {
            same_spec(session, spec)?;
            refresh(session, repo)
        },
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
        let two = LocalSpec::new("main..feature".into(), None);
        assert_eq!((two.base.as_str(), two.head.as_deref()), ("main", Some("feature")));
        let to_head = LocalSpec::new("main..".into(), None);
        assert_eq!((to_head.base.as_str(), to_head.head.as_deref()), ("main", Some("HEAD")));
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
        assert_eq!(id.split('/').count(), 3, "{id}");
        assert_eq!(id, session_id(Path::new("/repo"), &spec));
    }

    #[test]
    fn a_session_of_another_comparison_under_the_same_id_is_a_conflict() {
        use crate::model::{HeadRef, Repo, RevRef};
        let session = Session::new(
            "local/x/y".into(),
            Repo {
                root: "/repo".into(),
                host: None,
                owner: None,
                name: None,
                remote_url: None,
            },
            Source::Local {
                base_spec: "main".into(),
                head_spec: "worktree".into(),
                worktree: "/repo".into(),
            },
            RevRef {
                r#ref: Some("main".into()),
                sha: String::new(),
            },
            HeadRef {
                r#ref: None,
                sha: None,
                dirty: false,
            },
            "t".into(),
        );
        let spec = |base: &str| LocalSpec {
            base: base.into(),
            head: None,
        };
        assert!(same_spec(&session, &spec("main")).is_ok());
        assert_eq!(
            same_spec(&session, &spec("Main")).unwrap_err().code,
            crate::error::ErrorCode::EConflict
        );
    }

    #[test]
    fn session_ids_differ_for_specs_that_used_to_collide() {
        let id = |base: &str| {
            session_id(
                Path::new("/repo"),
                &LocalSpec {
                    base: base.into(),
                    head: None,
                },
            )
        };
        assert_ne!(id("a/b"), id("a__b"));
        assert_ne!(id("Feature"), id("feature"), "case-insensitive file systems");
        let with_head = session_id(
            Path::new("/repo"),
            &LocalSpec {
                base: "a".into(),
                head: Some("b".into()),
            },
        );
        assert_ne!(with_head, id("a"));
    }
}
