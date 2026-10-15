#![allow(dead_code)]

pub mod pr;

use std::path::{Path, PathBuf};
use std::process::Command as Process;

use assert_cmd::Command;

/// A throwaway repository with two commits, so HEAD~1..HEAD covers a
/// modification, an addition, a deletion and a rename.
pub struct Repo {
    dir: tempfile::TempDir,
}

impl Repo {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let repo = Self { dir };
        repo.git(&["init", "-q", "-b", "main"]);
        repo.write("keep.txt", "a\nb\nc\nd\ne\nf\ng\nh\ni\nj\n");
        repo.write("old.txt", "one\ntwo\nthree\n");
        repo.write("gone.txt", "gone\n");
        repo.git(&["add", "-A"]);
        repo.git(&["commit", "-q", "-m", "base"]);
        repo.write("keep.txt", "a\nb\nC\nd\ne\nf\ng\nh\ni\nj\nk\n");
        repo.git(&["mv", "old.txt", "new.txt"]);
        repo.git(&["rm", "-q", "gone.txt"]);
        repo.write("added.txt", "new1\nnew2");
        repo.git(&["add", "-A"]);
        repo.git(&["commit", "-q", "-m", "head"]);
        repo
    }

    pub fn path(&self) -> &Path {
        self.dir.path()
    }

    pub fn write(&self, name: &str, content: &str) {
        std::fs::write(self.path().join(name), content).unwrap();
    }

    pub fn git(&self, args: &[&str]) {
        let status = Process::new(real_git())
            .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
            .args(args)
            .current_dir(self.path())
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed");
    }
}

pub fn real_git() -> PathBuf {
    which::which("git").expect("git on PATH")
}

pub fn reviewkit(state: &tempfile::TempDir, repo: &Repo) -> Command {
    let mut cmd = Command::cargo_bin("reviewkit").unwrap();
    cmd.env("REVIEWKIT_GIT", real_git())
        .env("REVIEWKIT_GH", "/nonexistent/gh")
        .env("REVIEWKIT_HERDR", "/nonexistent/herdr")
        .arg("--state-dir")
        .arg(state.path())
        .arg("--cwd")
        .arg(repo.path());
    cmd
}

pub fn json(output: &std::process::Output) -> serde_json::Value {
    assert!(
        output.status.success(),
        "exit {:?}\nstdout: {}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("stdout is JSON")
}

pub fn target<'a>(session: &'a serde_json::Value, path: &str) -> &'a serde_json::Value {
    session["targets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["path"] == path)
        .unwrap_or_else(|| panic!("no target {path}"))
}

pub fn fixture_bin(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/bin")
        .join(name)
}
