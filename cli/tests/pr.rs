mod common;

use std::path::PathBuf;

use common::json;
use common::pr::{FakeGh, Remote, git_in, reviewkit, reviewkit_at, thread};

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
    git_in(
        remote.clone.path(),
        &["remote", "set-url", "origin", remote.bare.path().to_str().unwrap()],
    );
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
    git_in(
        remote.clone.path(),
        &["push", "-q", remote.bare.path().to_str().unwrap(), "main"],
    );
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
fn a_retargeted_pull_request_diffs_against_its_new_base() {
    let state = tempfile::tempdir().unwrap();
    let remote = Remote::new();
    let gh = FakeGh::new(&remote, "OPEN", serde_json::json!([]));
    json(
        &reviewkit(&state, &remote, &gh)
            .args(["session", "open", "--pr", "https://github.com/o/r/pull/7"])
            .output()
            .unwrap(),
    );
    git_in(
        remote.clone.path(),
        &[
            "push",
            "-q",
            remote.bare.path().to_str().unwrap(),
            "main~1:refs/heads/release",
        ],
    );
    let release = git_in(remote.clone.path(), &["rev-parse", "main~1"]);
    let rules = std::fs::read_to_string(gh.dir.path().join("responses.json"))
        .unwrap()
        .replace(&remote.base_sha, &release)
        .replace("\"baseRefName\":\"main\"", "\"baseRefName\":\"release\"");
    std::fs::write(gh.dir.path().join("responses.json"), rules).unwrap();
    let retargeted = json(
        &reviewkit(&state, &remote, &gh)
            .args(["session", "open", "--session", "pr/github.com/o/r/7"])
            .output()
            .unwrap(),
    );
    assert_eq!(retargeted["session"]["source"]["base_ref"], "release");
    assert_eq!(retargeted["session"]["base"]["sha"], release);
}

#[test]
fn a_pull_request_refresh_does_not_lock_out_other_commands_while_it_fetches() {
    let state = tempfile::tempdir().unwrap();
    let remote = Remote::new();
    let gh = FakeGh::new(&remote, "OPEN", serde_json::json!([]));
    json(
        &reviewkit(&state, &remote, &gh)
            .args(["session", "open", "--pr", "https://github.com/o/r/pull/7"])
            .output()
            .unwrap(),
    );
    let mut rules: Vec<serde_json::Value> =
        serde_json::from_str(&std::fs::read_to_string(gh.dir.path().join("responses.json")).unwrap()).unwrap();
    for rule in rules.iter_mut() {
        rule["sleep"] = serde_json::json!(3);
    }
    std::fs::write(
        gh.dir.path().join("responses.json"),
        serde_json::Value::Array(rules).to_string(),
    )
    .unwrap();

    let refresh = std::process::Command::new(assert_cmd::cargo::cargo_bin("reviewkit"))
        .envs(
            reviewkit(&state, &remote, &gh)
                .get_envs()
                .filter_map(|(k, v)| v.map(|v| (k.to_owned(), v.to_owned()))),
        )
        .args(["--state-dir", state.path().to_str().unwrap()])
        .args(["session", "refresh", "--session", "pr/github.com/o/r/7"])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1000));
    let decided = json(
        &reviewkit(&state, &remote, &gh)
            .args(["decision", "set", "--session", "pr/github.com/o/r/7"])
            .write_stdin(r#"{"event":"comment","body":"meanwhile"}"#)
            .output()
            .unwrap(),
    );
    assert_eq!(decided["session"]["decision"]["body"], "meanwhile");

    let refreshed = refresh.wait_with_output().unwrap();
    assert!(refreshed.status.success());
    let after: serde_json::Value = serde_json::from_slice(&refreshed.stdout).unwrap();
    assert_eq!(after["session"]["decision"]["body"], "meanwhile");
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

#[test]
fn a_hanging_fetch_times_out_instead_of_blocking() {
    let state = tempfile::tempdir().unwrap();
    let remote = Remote::new();
    let gh = FakeGh::new(&remote, "OPEN", serde_json::json!([]));
    let bin = tempfile::tempdir().unwrap();
    let slow_git = bin.path().join("git");
    std::fs::write(
        &slow_git,
        format!(
            "#!/bin/sh\ncase \"$*\" in *\" fetch \"*) sleep 30 ;; esac\nexec {} \"$@\"\n",
            common::real_git().display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&slow_git, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let started = std::time::Instant::now();
    let out = reviewkit(&state, &remote, &gh)
        .env("REVIEWKIT_GIT", &slow_git)
        .env("REVIEWKIT_GIT_TIMEOUT_MS", "500")
        .args(["session", "open", "--pr", "https://github.com/o/r/pull/7"])
        .output()
        .unwrap();
    assert!(started.elapsed() < std::time::Duration::from_secs(20));
    let body: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(body["error"]["code"], "E_GIT");
    assert!(body["error"]["message"].as_str().unwrap().contains("did not finish"));
}

#[test]
fn two_pull_requests_of_one_repository_can_be_opened_at_once() {
    let state = tempfile::tempdir().unwrap();
    let remote = Remote::new();
    let gh = FakeGh::new(&remote, "OPEN", serde_json::json!([]));
    git_in(
        remote.bare.path(),
        &["update-ref", "refs/pull/8/head", &remote.head_sha],
    );
    let mut rules: Vec<serde_json::Value> =
        serde_json::from_str(&std::fs::read_to_string(gh.dir.path().join("responses.json")).unwrap()).unwrap();
    let mut eight = rules[0].clone();
    eight["match"] = serde_json::json!(["pr view 8"]);
    eight["body"]["number"] = serde_json::json!(8);
    eight["body"]["url"] = serde_json::json!("https://github.com/o/r/pull/8");
    rules.insert(0, eight);
    for rule in rules.iter_mut() {
        rule["sleep"] = serde_json::json!(0.5);
    }
    std::fs::write(
        gh.dir.path().join("responses.json"),
        serde_json::Value::Array(rules).to_string(),
    )
    .unwrap();

    let spawn = |n: u32| {
        let mut cmd = std::process::Command::new(assert_cmd::cargo::cargo_bin("reviewkit"));
        let template = reviewkit(&state, &remote, &gh);
        for (k, v) in template.get_envs() {
            if let Some(v) = v {
                cmd.env(k, v);
            }
        }
        cmd.args(["--state-dir", state.path().to_str().unwrap()])
            .args(["session", "open", "--pr", &format!("https://github.com/o/r/pull/{n}")])
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap()
    };
    let (seven, eight) = (spawn(7), spawn(8));
    for child in [seven, eight] {
        let out = child.wait_with_output().unwrap();
        let body: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(body["ok"], true, "{body}");
    }
}

fn graphql_answers_with(gh: &FakeGh, edit: impl Fn(&mut serde_json::Value)) {
    let mut rules: Vec<serde_json::Value> =
        serde_json::from_str(&std::fs::read_to_string(gh.dir.path().join("responses.json")).unwrap()).unwrap();
    for rule in rules.iter_mut() {
        if rule["match"][0] == "api graphql" {
            edit(&mut rule["body"]);
        }
    }
    std::fs::write(
        gh.dir.path().join("responses.json"),
        serde_json::Value::Array(rules).to_string(),
    )
    .unwrap();
}

#[test]
fn a_pull_request_opens_when_only_an_optional_field_is_forbidden() {
    let state = tempfile::tempdir().unwrap();
    let remote = Remote::new();
    let gh = FakeGh::new(&remote, "OPEN", serde_json::json!([]));
    graphql_answers_with(&gh, |body| {
        body["data"]["repository"]["pullRequest"]["commits"]["nodes"][0]["commit"]["statusCheckRollup"] =
            serde_json::Value::Null;
        body["errors"] = serde_json::json!([{
            "type": "FORBIDDEN",
            "path": ["repository", "pullRequest", "commits", "nodes", 0, "commit", "statusCheckRollup"],
            "message": "Resource not accessible by integration"
        }]);
    });
    let body = json(
        &reviewkit(&state, &remote, &gh)
            .args(["session", "open", "--pr", "https://github.com/o/r/pull/7"])
            .output()
            .unwrap(),
    );
    assert_eq!(body["session"]["source"]["number"], 7);
}

#[test]
fn an_error_on_the_review_threads_still_fails_the_open() {
    let state = tempfile::tempdir().unwrap();
    let remote = Remote::new();
    let gh = FakeGh::new(&remote, "OPEN", serde_json::json!([]));
    graphql_answers_with(&gh, |body| {
        body["errors"] = serde_json::json!([{
            "path": ["repository", "pullRequest", "reviewThreads"],
            "message": "Something went wrong"
        }]);
    });
    let out = reviewkit(&state, &remote, &gh)
        .args(["session", "open", "--pr", "https://github.com/o/r/pull/7"])
        .output()
        .unwrap();
    assert!(!out.status.success());
}
