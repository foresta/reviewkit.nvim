#![allow(dead_code)]

use std::path::Path;
use std::process::Command as Process;

use super::{Repo, fixture_bin, real_git};

/// A bare "remote" with main and a PR branch exposed as refs/pull/7/head,
/// plus the user's clone of it, which does not have the PR's commits.
pub struct Remote {
    pub bare: tempfile::TempDir,
    pub clone: Repo,
    pub head_sha: String,
    pub base_sha: String,
}

pub fn git_in(dir: &Path, args: &[&str]) -> String {
    let out = Process::new(real_git())
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

impl Remote {
    pub fn new() -> Self {
        let clone = Repo::new();
        let bare = tempfile::tempdir().unwrap();
        git_in(bare.path(), &["init", "-q", "--bare", "-b", "main"]);
        // main = HEAD~1 + stable.txt; the PR branch = the clone's HEAD merged
        // with that main, so stable.txt exists at the head but is not in the
        // PR's diff.
        git_in(clone.path(), &["branch", "-f", "pr-branch", "HEAD"]);
        git_in(clone.path(), &["reset", "-q", "--hard", "HEAD~1"]);
        clone.write("stable.txt", "s1\ns2\ns3\n");
        git_in(clone.path(), &["add", "stable.txt"]);
        git_in(clone.path(), &["commit", "-q", "-m", "stable"]);
        git_in(clone.path(), &["checkout", "-q", "pr-branch"]);
        git_in(clone.path(), &["merge", "-q", "--no-edit", "main"]);
        git_in(clone.path(), &["checkout", "-q", "main"]);
        git_in(
            clone.path(),
            &["remote", "add", "origin", bare.path().to_str().unwrap()],
        );
        git_in(
            clone.path(),
            &["push", "-q", "origin", "main", "pr-branch:refs/pull/7/head"],
        );
        let head_sha = git_in(clone.path(), &["rev-parse", "pr-branch"]);
        let base_sha = git_in(clone.path(), &["rev-parse", "main"]);
        git_in(clone.path(), &["branch", "-D", "pr-branch"]);
        // A reviewer's checkout has never fetched someone else's PR; without
        // pruning, the objects made above would hide reads from the wrong repo.
        git_in(clone.path(), &["reflog", "expire", "--expire=now", "--all"]);
        git_in(clone.path(), &["gc", "-q", "--prune=now"]);
        git_in(
            clone.path(),
            &["remote", "set-url", "origin", "https://github.com/o/r.git"],
        );
        Self {
            bare,
            clone,
            head_sha,
            base_sha,
        }
    }
}

pub struct FakeGh {
    pub dir: tempfile::TempDir,
}

impl FakeGh {
    pub fn new(remote: &Remote, state: &str, threads: serde_json::Value) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let view = serde_json::json!({
            "number": 7, "url": "https://github.com/o/r/pull/7", "title": "Add k", "author": {"login": "alice"},
            "state": state, "headRefName": "feature", "baseRefName": "main",
            "baseRefOid": remote.base_sha, "headRefOid": remote.head_sha, "id": "PR_node"
        });
        let graphql = serde_json::json!({ "data": { "repository": { "pullRequest": {
            "id": "PR_node", "title": "Add k", "body": "Adds k.", "createdAt": "2026-01-01T00:00:00Z",
            "updatedAt": "2026-01-02T00:00:00Z", "url": "https://github.com/o/r/pull/7", "author": {"login": "alice"},
            "reviewThreads": { "pageInfo": { "hasNextPage": false, "endCursor": null }, "nodes": threads },
            "reviews": { "nodes": [ { "state": "COMMENTED", "body": "looks ok", "submittedAt": "2026-01-03T00:00:00Z", "url": "r", "author": {"login": "bob"} } ] },
            "comments": { "nodes": [] },
            "commits": { "nodes": [ { "commit": { "oid": remote.head_sha, "statusCheckRollup": { "state": "SUCCESS", "contexts": { "nodes": [
                { "__typename": "CheckRun", "name": "ci", "status": "COMPLETED", "conclusion": "SUCCESS", "detailsUrl": "d" }
            ] } } } } ] }
        } } } });
        let rules = serde_json::json!([
            { "match": ["pr view 7"], "body": view },
            { "match": ["api graphql"], "body": graphql },
        ]);
        std::fs::write(dir.path().join("responses.json"), rules.to_string()).unwrap();
        Self { dir }
    }

    pub fn calls(&self) -> Vec<serde_json::Value> {
        std::fs::read_to_string(self.dir.path().join("calls.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }
}

/// The clone's origin is `https://github.com/o/r.git`, like a real checkout
/// of the PR's repository, so the mirror is seeded from it; git's
/// `url.<bare>.insteadOf` (via GIT_CONFIG_*) points that URL back at the
/// bare repository for every fetch.
pub fn reviewkit(state: &tempfile::TempDir, remote: &Remote, gh: &FakeGh) -> assert_cmd::Command {
    let mut cmd = reviewkit_at(state, remote, gh);
    cmd.arg("--cwd").arg(remote.clone.path());
    cmd
}

pub fn reviewkit_at(state: &tempfile::TempDir, remote: &Remote, gh: &FakeGh) -> assert_cmd::Command {
    let mut cmd = assert_cmd::Command::cargo_bin("reviewkit").unwrap();
    cmd.env("REVIEWKIT_GIT", real_git())
        .env("REVIEWKIT_GH", fixture_bin("gh"))
        .env("REVIEWKIT_HERDR", "/nonexistent/herdr")
        .env("FAKE_GH_DIR", gh.dir.path())
        .env("GIT_CONFIG_COUNT", "1")
        .env(
            "GIT_CONFIG_KEY_0",
            format!("url.{}.insteadOf", remote.bare.path().display()),
        )
        .env("GIT_CONFIG_VALUE_0", "https://github.com/o/r.git")
        .arg("--state-dir")
        .arg(state.path());
    cmd
}

pub fn thread(path: &str, line: Option<u32>, side: &str, outdated: bool) -> serde_json::Value {
    serde_json::json!({
        "id": format!("PRRT_{path}"), "path": path, "line": line, "startLine": null, "originalLine": line.unwrap_or(1),
        "diffSide": side, "startDiffSide": null, "isResolved": false, "isOutdated": outdated, "subjectType": "LINE",
        "comments": { "nodes": [ { "id": "PRRC_1", "body": "hm", "createdAt": "t", "url": "u", "author": {"login": "bob"} } ] }
    })
}
