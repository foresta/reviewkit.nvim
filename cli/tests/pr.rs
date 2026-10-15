mod common;

use std::path::{Path, PathBuf};
use std::process::Command as Process;

use common::{Repo, fixture_bin, json, real_git};

/// A bare "remote" with main and a PR branch exposed as refs/pull/7/head,
/// plus the user's clone of it. The clone's origin is the bare path, so the
/// mirror can fetch without a network.
struct Remote {
    bare: tempfile::TempDir,
    clone: Repo,
    head_sha: String,
    base_sha: String,
}

fn git_in(dir: &Path, args: &[&str]) -> String {
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
    fn new() -> Self {
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
        Self {
            bare,
            clone,
            head_sha,
            base_sha,
        }
    }
}

struct FakeGh {
    dir: tempfile::TempDir,
}

impl FakeGh {
    fn new(remote: &Remote, state: &str, threads: serde_json::Value) -> Self {
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

    fn calls(&self) -> Vec<serde_json::Value> {
        std::fs::read_to_string(self.dir.path().join("calls.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }
}

/// The clone's origin is a local bare path, not GitHub, so the mirror is
/// created empty with `https://github.com/o/r.git` as its remote; git's
/// `url.<bare>.insteadOf` (via GIT_CONFIG_*) points that URL back at the
/// bare repository for the fetch.
fn reviewkit(state: &tempfile::TempDir, remote: &Remote, gh: &FakeGh) -> assert_cmd::Command {
    let mut cmd = reviewkit_at(state, remote, gh);
    cmd.arg("--cwd").arg(remote.clone.path());
    cmd
}

fn reviewkit_at(state: &tempfile::TempDir, remote: &Remote, gh: &FakeGh) -> assert_cmd::Command {
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

fn thread(path: &str, line: Option<u32>, side: &str, outdated: bool) -> serde_json::Value {
    serde_json::json!({
        "id": format!("PRRT_{path}"), "path": path, "line": line, "startLine": null, "originalLine": line.unwrap_or(1),
        "diffSide": side, "startDiffSide": null, "isResolved": false, "isOutdated": outdated, "subjectType": "LINE",
        "comments": { "nodes": [ { "id": "PRRC_1", "body": "hm", "createdAt": "t", "url": "u", "author": {"login": "bob"} } ] }
    })
}

#[test]
fn open_pr_by_url_materializes_a_worktree_and_builds_targets_and_threads() {
    let state = tempfile::tempdir().unwrap();
    let remote = Remote::new();
    let gh = FakeGh::new(
        &remote,
        "OPEN",
        serde_json::json!([
            thread("keep.txt", Some(3), "RIGHT", false),
            thread("gone.txt", None, "LEFT", true)
        ]),
    );
    let body = json(
        &reviewkit(&state, &remote, &gh)
            .args(["session", "open", "--pr", "https://github.com/o/r/pull/7"])
            .output()
            .unwrap(),
    );
    let session = &body["session"];
    assert_eq!(session["id"], "pr/github.com/o/r/7");
    assert_eq!(session["source"]["kind"], "pr");
    assert_eq!(session["source"]["title"], "Add k");
    assert_eq!(session["source"]["state"], "OPEN");
    assert_eq!(session["head"]["sha"], remote.head_sha);
    assert_eq!(
        session["base"]["sha"], remote.base_sha,
        "merge-base of the PR head and main"
    );
    let worktree = PathBuf::from(session["source"]["worktree"].as_str().unwrap());
    assert!(worktree.starts_with(state.path()), "worktree lives under the state dir");
    assert!(worktree.join("added.txt").exists());
    assert!(
        !remote.clone.path().join(".git/worktrees").exists(),
        "the user's repo is untouched"
    );
    let paths: Vec<&str> = session["targets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["path"].as_str().unwrap())
        .collect();
    assert_eq!(paths, vec!["added.txt", "gone.txt", "keep.txt", "new.txt"]);
    assert_eq!(session["threads"].as_array().unwrap().len(), 2);
    assert_eq!(session["threads"][1]["side"], "base");
    assert_eq!(session["threads"][1]["line"], serde_json::Value::Null);
    assert_eq!(session["threads"][0]["comments"][0]["author"], "bob");
    assert_eq!(session["repo"]["host"], "github.com");

    let info = json(
        &reviewkit(&state, &remote, &gh)
            .args(["pr", "show", "--session", "pr/github.com/o/r/7"])
            .output()
            .unwrap(),
    );
    assert_eq!(info["data"]["title"], "Add k");
    assert_eq!(info["data"]["timeline"][0]["kind"], "review");
    assert_eq!(info["data"]["checks"][0]["conclusion"], "SUCCESS");
    assert!(state.path().join("sessions/pr/github.com/o/r/7/pr.json").exists());
}

#[test]
fn a_thread_on_an_unchanged_file_becomes_a_thread_target_and_a_missing_file_does_not() {
    let state = tempfile::tempdir().unwrap();
    let remote = Remote::new();
    let gh = FakeGh::new(
        &remote,
        "OPEN",
        serde_json::json!([
            thread("stable.txt", Some(2), "RIGHT", false),
            thread("nope.txt", Some(1), "RIGHT", false)
        ]),
    );
    let body = json(
        &reviewkit(&state, &remote, &gh)
            .args(["session", "open", "--pr", "https://github.com/o/r/pull/7"])
            .output()
            .unwrap(),
    );
    let targets = body["session"]["targets"].as_array().unwrap();
    let stable = targets
        .iter()
        .find(|t| t["path"] == "stable.txt")
        .expect("thread target");
    assert_eq!(stable["reason"], "thread");
    assert_eq!(stable["range"], serde_json::json!({ "start": 2, "end": 2 }));
    assert!(stable["hunks"].as_array().unwrap().is_empty());
    assert_eq!(stable["head_blob"].as_str().unwrap().len(), 40);
    assert!(targets.iter().all(|t| t["path"] != "nope.txt"));
    assert_eq!(
        body["session"]["threads"].as_array().unwrap().len(),
        2,
        "threads are kept even without a target"
    );
}

#[test]
fn reopening_rereads_metadata_and_resume_works() {
    let state = tempfile::tempdir().unwrap();
    let remote = Remote::new();
    let gh = FakeGh::new(&remote, "OPEN", serde_json::json!([]));
    json(
        &reviewkit(&state, &remote, &gh)
            .args(["session", "open", "--pr", "https://github.com/o/r/pull/7"])
            .output()
            .unwrap(),
    );
    let calls_before = gh.calls().len();
    let again = json(
        &reviewkit(&state, &remote, &gh)
            .args(["session", "open", "--session", "pr/github.com/o/r/7"])
            .output()
            .unwrap(),
    );
    assert_eq!(again["session"]["rev"], 2);
    assert!(gh.calls().len() > calls_before, "metadata is re-read on resume");
    let listed = json(
        &reviewkit(&state, &remote, &gh)
            .args(["session", "list"])
            .output()
            .unwrap(),
    );
    assert_eq!(listed["data"][0]["kind"], "pr");
    assert_eq!(listed["data"][0]["title"], "Add k");
}

#[test]
fn pr_number_without_a_github_origin_is_invalid_and_merged_prs_use_the_recorded_base() {
    let state = tempfile::tempdir().unwrap();
    let remote = Remote::new();
    let gh = FakeGh::new(&remote, "MERGED", serde_json::json!([]));
    let out = reviewkit(&state, &remote, &gh)
        .args(["session", "open", "--pr", "7"])
        .output()
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        body["error"]["code"], "E_INVALID",
        "origin is a local path, not a GitHub URL"
    );

    let body = json(
        &reviewkit(&state, &remote, &gh)
            .args(["session", "open", "--pr", "https://github.com/o/r/pull/7"])
            .output()
            .unwrap(),
    );
    assert_eq!(body["session"]["source"]["state"], "MERGED");
    assert_eq!(body["session"]["base"]["sha"], remote.base_sha);
    assert!(!body["session"]["targets"].as_array().unwrap().is_empty());
}

#[test]
fn close_purge_removes_the_worktree_but_keeps_the_mirror() {
    let state = tempfile::tempdir().unwrap();
    let remote = Remote::new();
    let gh = FakeGh::new(&remote, "OPEN", serde_json::json!([]));
    let body = json(
        &reviewkit(&state, &remote, &gh)
            .args(["session", "open", "--pr", "https://github.com/o/r/pull/7"])
            .output()
            .unwrap(),
    );
    let worktree = PathBuf::from(body["session"]["source"]["worktree"].as_str().unwrap());
    assert!(worktree.exists());
    json(
        &reviewkit(&state, &remote, &gh)
            .args(["session", "close", "--session", "pr/github.com/o/r/7", "--purge"])
            .output()
            .unwrap(),
    );
    assert!(!worktree.exists());
    assert!(state.path().join("repos/github.com/o/r/mirror.git/HEAD").exists());
    assert!(!state.path().join("sessions/pr/github.com/o/r/7").exists());
}

#[test]
fn gh_auth_failure_is_e_gh_auth() {
    let state = tempfile::tempdir().unwrap();
    let remote = Remote::new();
    let gh = FakeGh::new(&remote, "OPEN", serde_json::json!([]));
    std::fs::write(
        gh.dir.path().join("responses.json"),
        serde_json::json!([{ "match": ["pr view"], "stderr": "To get started with GitHub CLI, please run:  gh auth login\n", "exit": 4 }]).to_string(),
    )
    .unwrap();
    let out = reviewkit(&state, &remote, &gh)
        .args(["session", "open", "--pr", "https://github.com/o/r/pull/7"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(4));
    let body: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(body["error"]["code"], "E_GH_AUTH");
    assert!(
        !state.path().join("sessions").exists(),
        "nothing is created before gh answers"
    );
}

#[test]
fn a_url_can_be_opened_outside_any_checkout_and_ghe_hosts_reach_gh() {
    let state = tempfile::tempdir().unwrap();
    let remote = Remote::new();
    let gh = FakeGh::new(&remote, "OPEN", serde_json::json!([]));
    let elsewhere = tempfile::tempdir().unwrap();
    let body = json(
        &reviewkit_at(&state, &remote, &gh)
            .arg("--cwd")
            .arg(elsewhere.path())
            .args(["session", "open", "--pr", "https://github.com/o/r/pull/7"])
            .output()
            .unwrap(),
    );
    assert_eq!(body["session"]["id"], "pr/github.com/o/r/7");
    assert!(!body["session"]["targets"].as_array().unwrap().is_empty());

    // Same PR on an enterprise host: gh gets --repo host/o/r and --hostname,
    // and reports the PR URL on that host.
    let rules = std::fs::read_to_string(gh.dir.path().join("responses.json"))
        .unwrap()
        .replace("https://github.com/o/r/pull/7", "https://ghe.example.com/o/r/pull/7");
    std::fs::write(gh.dir.path().join("responses.json"), rules).unwrap();
    let out = reviewkit_at(&state, &remote, &gh)
        .arg("--cwd")
        .arg(elsewhere.path())
        .env("GIT_CONFIG_VALUE_0", "https://ghe.example.com/o/r.git")
        .args(["session", "open", "--pr", "https://ghe.example.com/o/r/pull/7"])
        .output()
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(body["session"]["id"], "pr/ghe.example.com/o/r/7", "{}", body);
    let calls = gh.calls();
    assert!(
        calls
            .iter()
            .any(|c| c["args"].as_array().unwrap().iter().any(|a| a == "ghe.example.com/o/r"))
    );
    assert!(
        calls
            .iter()
            .any(|c| c["args"].as_array().unwrap().iter().any(|a| a == "--hostname"))
    );
}

#[test]
fn review_threads_are_paginated() {
    let state = tempfile::tempdir().unwrap();
    let remote = Remote::new();
    let gh = FakeGh::new(
        &remote,
        "OPEN",
        serde_json::json!([thread("keep.txt", Some(3), "RIGHT", false)]),
    );
    let rules: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(gh.dir.path().join("responses.json")).unwrap()).unwrap();
    let mut first = rules[1].clone();
    first["body"]["data"]["repository"]["pullRequest"]["reviewThreads"]["pageInfo"] =
        serde_json::json!({ "hasNextPage": true, "endCursor": "C1" });
    let mut second = rules[1].clone();
    second["match"] = serde_json::json!(["api graphql", "\"cursor\":\"C1\""]);
    second["body"]["data"]["repository"]["pullRequest"]["reviewThreads"]["nodes"] =
        serde_json::json!([thread("added.txt", Some(1), "RIGHT", false)]);
    let paged = serde_json::json!([rules[0], second, first]);
    std::fs::write(gh.dir.path().join("responses.json"), paged.to_string()).unwrap();
    let body = json(
        &reviewkit(&state, &remote, &gh)
            .args(["session", "open", "--pr", "https://github.com/o/r/pull/7"])
            .output()
            .unwrap(),
    );
    let paths: Vec<&str> = body["session"]["threads"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["path"].as_str().unwrap())
        .collect();
    assert_eq!(paths, vec!["keep.txt", "added.txt"]);
}

#[test]
fn reopen_skips_the_fetch_when_nothing_moved_and_fetches_when_the_base_moves() {
    let state = tempfile::tempdir().unwrap();
    let remote = Remote::new();
    let gh = FakeGh::new(&remote, "OPEN", serde_json::json!([]));
    json(
        &reviewkit(&state, &remote, &gh)
            .args(["session", "open", "--pr", "https://github.com/o/r/pull/7"])
            .output()
            .unwrap(),
    );
    // With the remote unreachable, a reopen must not need to fetch.
    let again = json(
        &reviewkit(&state, &remote, &gh)
            .env("GIT_CONFIG_VALUE_0", "https://unreachable.invalid/o/r.git")
            .args(["session", "open", "--session", "pr/github.com/o/r/7"])
            .output()
            .unwrap(),
    );
    assert_eq!(again["session"]["rev"], 2);

    // main moves on the remote; GitHub reports the new base oid; the mirror
    // must fetch it even though the PR head is unchanged.
    git_in(
        remote.clone.path(),
        &["commit", "-q", "--allow-empty", "-m", "base moved"],
    );
    git_in(remote.clone.path(), &["push", "-q", "origin", "main"]);
    let new_base = git_in(remote.clone.path(), &["rev-parse", "main"]);
    let rules = std::fs::read_to_string(gh.dir.path().join("responses.json"))
        .unwrap()
        .replace(&remote.base_sha, &new_base);
    std::fs::write(gh.dir.path().join("responses.json"), rules).unwrap();
    let moved = json(
        &reviewkit(&state, &remote, &gh)
            .args(["session", "open", "--session", "pr/github.com/o/r/7"])
            .output()
            .unwrap(),
    );
    assert_eq!(moved["session"]["source"]["base_oid"], new_base);
    assert_eq!(
        moved["session"]["base"]["sha"], remote.base_sha,
        "merge-base stays at the fork point"
    );
}

#[test]
fn a_deleted_mirror_is_recreated_on_reopen() {
    let state = tempfile::tempdir().unwrap();
    let remote = Remote::new();
    let gh = FakeGh::new(&remote, "OPEN", serde_json::json!([]));
    let body = json(
        &reviewkit(&state, &remote, &gh)
            .args(["session", "open", "--pr", "https://github.com/o/r/pull/7"])
            .output()
            .unwrap(),
    );
    let worktree = PathBuf::from(body["session"]["source"]["worktree"].as_str().unwrap());
    std::fs::remove_dir_all(state.path().join("repos/github.com/o/r/mirror.git")).unwrap();
    std::fs::write(worktree.join("scratch.txt"), "leftover").unwrap();
    let again = json(
        &reviewkit(&state, &remote, &gh)
            .args(["session", "open", "--session", "pr/github.com/o/r/7"])
            .output()
            .unwrap(),
    );
    assert_eq!(again["session"]["rev"], 2);
    assert!(worktree.join("added.txt").exists());
}

#[test]
fn a_failed_first_open_leaves_no_session_directory() {
    let state = tempfile::tempdir().unwrap();
    let remote = Remote::new();
    let gh = FakeGh::new(&remote, "OPEN", serde_json::json!([]));
    let rules = serde_json::json!([
        serde_json::from_str::<serde_json::Value>(&std::fs::read_to_string(gh.dir.path().join("responses.json")).unwrap()).unwrap()[0],
        { "match": ["api graphql"], "raw": "{\"errors\":[{\"message\":\"Could not resolve to a PullRequest\"}]}", "exit": 1 }
    ]);
    std::fs::write(gh.dir.path().join("responses.json"), rules.to_string()).unwrap();
    let out = reviewkit(&state, &remote, &gh)
        .args(["session", "open", "--pr", "https://github.com/o/r/pull/7"])
        .output()
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(body["error"]["code"], "E_GH");
    assert_eq!(body["error"]["retryable"], false);
    assert!(body["error"]["message"].as_str().unwrap().contains("Could not resolve"));
    assert!(!state.path().join("sessions/pr").exists());
}

#[test]
fn a_checkout_of_the_same_repository_seeds_the_mirror_without_network() {
    let state = tempfile::tempdir().unwrap();
    let remote = Remote::new();
    let gh = FakeGh::new(&remote, "OPEN", serde_json::json!([]));
    // The checkout's origin is the GitHub URL itself; the insteadOf rewrite
    // only redirects fetches to the bare repository.
    git_in(
        remote.clone.path(),
        &["remote", "set-url", "origin", "https://github.com/o/r.git"],
    );
    let body = json(
        &reviewkit(&state, &remote, &gh)
            .args(["session", "open", "--pr", "7"])
            .output()
            .unwrap(),
    );
    let session = &body["session"];
    assert_eq!(session["id"], "pr/github.com/o/r/7");
    assert_eq!(
        session["repo"]["root"],
        remote.clone.path().canonicalize().unwrap().to_str().unwrap()
    );
    assert_eq!(session["repo"]["remote_url"], "https://github.com/o/r.git");
    assert!(!remote.clone.path().join(".git/worktrees").exists());
    // Seeded from the checkout: a commit only the checkout has is already in the mirror.
    let mirror = state.path().join("repos/github.com/o/r/mirror.git");
    let local_only = git_in(remote.clone.path(), &["rev-parse", "HEAD"]);
    git_in(&mirror, &["cat-file", "-e", &format!("{local_only}^{{commit}}")]);
    assert_eq!(
        git_in(&mirror, &["config", "--get", "remote.origin.url"]),
        "https://github.com/o/r.git"
    );
}

#[test]
fn owner_and_name_casing_follows_github_not_the_argument() {
    let state = tempfile::tempdir().unwrap();
    let remote = Remote::new();
    let gh = FakeGh::new(&remote, "OPEN", serde_json::json!([]));
    let body = json(
        &reviewkit(&state, &remote, &gh)
            .args(["session", "open", "--pr", "https://github.com/O/R/pull/7"])
            .output()
            .unwrap(),
    );
    assert_eq!(
        body["session"]["id"], "pr/github.com/o/r/7",
        "gh reported the URL as o/r"
    );
    assert!(state.path().join("repos/github.com/o/r/mirror.git").exists());
    // On a case-insensitive filesystem `O` resolves to `o`, so the listing
    // is what shows that only one owner directory was created.
    let owners: Vec<String> = std::fs::read_dir(state.path().join("repos/github.com"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(owners, vec!["o"]);
}
