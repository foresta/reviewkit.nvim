use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

#[derive(Debug, Clone)]
pub struct StateDir(PathBuf);

impl StateDir {
    pub fn new(path: PathBuf) -> Self {
        Self(path)
    }

    /// Made absolute so `state_path` stays valid for an editor whose cwd
    /// differs from the one this command ran in.
    pub fn resolve(flag: Option<PathBuf>) -> Result<Self> {
        let dir = flag
            .or_else(|| env_non_empty("REVIEWKIT_STATE_DIR").map(PathBuf::from))
            .or_else(|| env_non_empty("XDG_STATE_HOME").map(|dir| Path::new(&dir).join("reviewkit")))
            .or_else(|| std::env::home_dir().map(|home| home.join(".local").join("state").join("reviewkit")))
            .ok_or_else(|| {
                Error::invalid("cannot determine the state directory").hint("set --state-dir or $REVIEWKIT_STATE_DIR")
            })?;
        Ok(Self(std::path::absolute(dir)?))
    }

    pub fn as_path(&self) -> &Path {
        &self.0
    }

    pub fn sessions_dir(&self) -> PathBuf {
        self.0.join("sessions")
    }

    pub fn repos_dir(&self) -> PathBuf {
        self.0.join("repos")
    }

    pub fn mirror_dir(&self, host: &str, owner: &str, name: &str) -> PathBuf {
        self.repos_dir().join(host).join(owner).join(name).join("mirror.git")
    }

    pub fn pr_worktree(&self, host: &str, owner: &str, name: &str, number: u64) -> PathBuf {
        self.repos_dir()
            .join(host)
            .join(owner)
            .join(name)
            .join("pr")
            .join(number.to_string())
    }

    /// Session ids are slash-separated (`pr/<host>/<owner>/<repo>/<n>`) and map
    /// to nested directories, so each component is validated as a plain
    /// directory name.
    pub fn session_dir(&self, id: &str) -> Result<PathBuf> {
        let mut dir = self.sessions_dir();
        let components: Vec<&str> = id.split('/').collect();
        if components.is_empty()
            || components
                .iter()
                .any(|c| c.is_empty() || *c == "." || *c == ".." || c.contains('\\'))
        {
            return Err(Error::invalid(format!("invalid session id: {id:?}")));
        }
        for component in components {
            dir.push(component);
        }
        Ok(dir)
    }

    pub fn session_file(&self, id: &str) -> Result<PathBuf> {
        Ok(self.session_dir(id)?.join("session.json"))
    }

    pub fn pr_file(&self, id: &str) -> Result<PathBuf> {
        Ok(self.session_dir(id)?.join("pr.json"))
    }

    pub fn lock_file(&self, id: &str) -> Result<PathBuf> {
        Ok(self.session_dir(id)?.join("session.lock"))
    }

    pub fn fetch_lock_file(&self, id: &str) -> Result<PathBuf> {
        Ok(self.session_dir(id)?.join("fetch.lock"))
    }
}

/// Empty counts as unset so that `VAR= reviewkit ...` behaves like an unset
/// VAR for every variable, instead of each call site deciding differently.
pub fn env_non_empty(name: &str) -> Option<std::ffi::OsString> {
    std::env::var_os(name).filter(|v| !v.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_id_components_become_nested_directories() {
        let dir = StateDir::new(PathBuf::from("/s"));
        assert_eq!(
            dir.session_file("pr/github.com/o/r/7").unwrap(),
            PathBuf::from("/s/sessions/pr/github.com/o/r/7/session.json")
        );
    }

    #[test]
    fn session_id_cannot_escape_the_sessions_directory() {
        let dir = StateDir::new(PathBuf::from("/s"));
        for bad in ["", "a//b", "../x", "a/./b", "a\\b", "/abs"] {
            assert!(dir.session_dir(bad).is_err(), "{bad:?} should be rejected");
        }
    }
}
