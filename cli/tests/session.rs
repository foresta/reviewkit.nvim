mod common;

use assert_cmd::Command;
use common::{Repo, json, real_git, reviewkit, target};
use std::process::Command as Process;

#[test]
fn open_local_between_two_commits_lists_every_change() {
    let state = tempfile::tempdir().unwrap();
    let repo = Repo::new();
    let out = reviewkit(&state, &repo)
        .args(["session", "open", "--local", "--base", "HEAD~1", "--head", "HEAD"])
        .output()
        .unwrap();
    let body = json(&out);
    let session = &body["session"];
    assert_eq!(body["data"], serde_json::Value::Null);
    assert_eq!(session["rev"], 1);
    assert_eq!(session["source"]["kind"], "local");
    assert_eq!(session["source"]["head_spec"], "HEAD");
    assert_eq!(session["head"]["dirty"], false);
    assert_eq!(session["base"]["sha"].as_str().unwrap().len(), 40);
    assert_eq!(session["head"]["sha"].as_str().unwrap().len(), 40);

    let keep = target(session, "keep.txt");
    assert_eq!(keep["status"], "modified");
    assert_eq!(keep["hunks"].as_array().unwrap().len(), 2);
    assert_eq!(
        keep["commentable"]["head"],
        serde_json::json!([[1, 6], [8, 11]]),
        "line 7 is outside both hunks"
    );
    assert_eq!(keep["head_blob"].as_str().unwrap().len(), 40);
    assert_eq!(target(session, "added.txt")["status"], "added");
    assert_eq!(target(session, "gone.txt")["status"], "deleted");
    let renamed = target(session, "new.txt");
    assert_eq!(renamed["status"], "renamed");
    assert_eq!(renamed["old_path"], "old.txt");
    assert_eq!(
        renamed["base_blob"].as_str().unwrap().len(),
        40,
        "100% renames still get blobs"
    );
}

#[test]
fn open_local_against_the_worktree_includes_untracked_and_is_dirty() {
    let state = tempfile::tempdir().unwrap();
    let repo = Repo::new();
    repo.write("keep.txt", "a\nb\nC\nd\ne\nf\ng\nh\ni\nj\nk\nz\n");
    repo.write("scratch.txt", "x\ny");
    let out = reviewkit(&state, &repo)
        .args(["session", "open", "--local"])
        .output()
        .unwrap();
    let session = &json(&out)["session"];
    assert_eq!(session["source"]["base_spec"], "HEAD");
    assert_eq!(session["source"]["head_spec"], "worktree");
    assert_eq!(session["head"]["sha"], serde_json::Value::Null);
    assert_eq!(session["head"]["dirty"], true);
    let scratch = target(session, "scratch.txt");
    assert_eq!(scratch["status"], "untracked");
    assert_eq!(
        scratch["hunks"],
        serde_json::json!([{"base_start": 0, "base_count": 0, "head_start": 1, "head_count": 2}])
    );
    assert_eq!(scratch["commentable"]["head"], serde_json::json!([[1, 2]]));
    assert_eq!(
        target(session, "keep.txt")["hunks"][0],
        serde_json::json!({"base_start": 11, "base_count": 0, "head_start": 12, "head_count": 1})
    );
}

#[test]
fn reopening_resumes_and_refreshes_the_same_session() {
    let state = tempfile::tempdir().unwrap();
    let repo = Repo::new();
    let first = json(
        &reviewkit(&state, &repo)
            .args(["session", "open", "--local"])
            .output()
            .unwrap(),
    );
    let id = first["session"]["id"].as_str().unwrap().to_string();
    assert!(first["session"]["targets"].as_array().unwrap().is_empty());

    repo.write("keep.txt", "changed\n");
    let second = json(
        &reviewkit(&state, &repo)
            .args(["session", "open", "--local"])
            .output()
            .unwrap(),
    );
    assert_eq!(second["session"]["id"], id);
    assert_eq!(second["session"]["rev"], 2);
    assert_eq!(target(&second["session"], "keep.txt")["status"], "modified");

    let resumed = json(
        &reviewkit(&state, &repo)
            .args(["session", "open", "--session", &id])
            .output()
            .unwrap(),
    );
    assert_eq!(
        resumed["session"]["rev"], 2,
        "nothing changed since, so nothing was written"
    );
}

#[test]
fn get_list_target_list_and_close_purge() {
    let state = tempfile::tempdir().unwrap();
    let repo = Repo::new();
    let opened = json(
        &reviewkit(&state, &repo)
            .args(["session", "open", "--local", "--base", "HEAD~1"])
            .output()
            .unwrap(),
    );
    let id = opened["session"]["id"].as_str().unwrap().to_string();

    let got = json(
        &reviewkit(&state, &repo)
            .args(["session", "get", "--session", &id])
            .output()
            .unwrap(),
    );
    assert_eq!(got["session"]["id"], id);

    let listed = json(&reviewkit(&state, &repo).args(["session", "list"]).output().unwrap());
    assert_eq!(listed["data"][0]["id"], id);
    assert_eq!(listed["data"][0]["kind"], "local");

    let targets = json(
        &reviewkit(&state, &repo)
            .args(["target", "list", "--session", &id])
            .output()
            .unwrap(),
    );
    assert!(
        targets["data"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["path"] == "keep.txt")
    );

    let closed = json(
        &reviewkit(&state, &repo)
            .args(["session", "close", "--session", &id, "--purge"])
            .output()
            .unwrap(),
    );
    assert_eq!(closed["data"]["closed"], true);
    let gone = reviewkit(&state, &repo)
        .args(["session", "get", "--session", &id])
        .output()
        .unwrap();
    assert_eq!(gone.status.code(), Some(3));
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&gone.stdout).unwrap()["error"]["code"],
        "E_NO_SESSION"
    );
}

#[test]
fn file_base_returns_lines_or_exists_false() {
    let state = tempfile::tempdir().unwrap();
    let repo = Repo::new();
    let opened = json(
        &reviewkit(&state, &repo)
            .args(["session", "open", "--local", "--base", "HEAD~1"])
            .output()
            .unwrap(),
    );
    let id = opened["session"]["id"].as_str().unwrap().to_string();

    let old = json(
        &reviewkit(&state, &repo)
            .args(["file", "base", "--session", &id, "--path", "old.txt"])
            .output()
            .unwrap(),
    );
    assert_eq!(
        old["data"],
        serde_json::json!({"exists": true, "binary": false, "too_large": false, "lines": ["one", "two", "three"]})
    );
    let added = json(
        &reviewkit(&state, &repo)
            .args(["file", "base", "--session", &id, "--path", "added.txt"])
            .output()
            .unwrap(),
    );
    assert_eq!(added["data"]["exists"], false);
}

#[test]
fn unknown_base_is_invalid_and_outside_a_repo_is_e_git() {
    let state = tempfile::tempdir().unwrap();
    let repo = Repo::new();
    let out = reviewkit(&state, &repo)
        .args(["session", "open", "--local", "--base", "nope"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let plain = tempfile::tempdir().unwrap();
    let out = Command::cargo_bin("reviewkit")
        .unwrap()
        .env("REVIEWKIT_GIT", real_git())
        .arg("--state-dir")
        .arg(state.path())
        .arg("--cwd")
        .arg(plain.path())
        .args(["session", "open", "--local"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(4));
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&out.stdout).unwrap()["error"]["code"],
        "E_GIT"
    );
}

#[test]
fn user_diff_config_and_odd_paths_do_not_break_targets() {
    let state = tempfile::tempdir().unwrap();
    let repo = Repo::new();
    repo.git(&["config", "diff.noprefix", "true"]);
    repo.git(&["config", "diff.mnemonicPrefix", "true"]);
    repo.git(&["config", "diff.suppressBlankEmpty", "true"]);
    repo.write("sp ace.txt", "a\n\nb\n");
    repo.write("quo\"te.txt", "q\n");
    repo.git(&["add", "-A"]);
    repo.git(&["commit", "-q", "-m", "odd"]);
    repo.write("sp ace.txt", "a\n\nB\n");
    repo.write("quo\"te.txt", "Q\n");
    let session = &json(
        &reviewkit(&state, &repo)
            .args(["session", "open", "--local"])
            .output()
            .unwrap(),
    )["session"];
    let spaced = target(session, "sp ace.txt");
    assert_eq!(
        spaced["hunks"],
        serde_json::json!([{"base_start": 3, "base_count": 1, "head_start": 3, "head_count": 1}])
    );
    assert_eq!(target(session, "quo\"te.txt")["stats"]["added"], 1);
}

#[test]
fn mode_plus_content_is_modified_and_a_typechange_is_one_target() {
    let state = tempfile::tempdir().unwrap();
    let repo = Repo::new();
    repo.write("mode.txt", "m\n");
    std::os::unix::fs::symlink("keep.txt", repo.path().join("link")).unwrap();
    repo.git(&["add", "-A"]);
    repo.git(&["commit", "-q", "-m", "before"]);
    repo.write("mode.txt", "M\n");
    std::fs::set_permissions(
        repo.path().join("mode.txt"),
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .unwrap();
    std::fs::remove_file(repo.path().join("link")).unwrap();
    repo.write("link", "now a file\n");
    repo.git(&["add", "-A"]);
    repo.git(&["commit", "-q", "-m", "after"]);
    let session = &json(
        &reviewkit(&state, &repo)
            .args(["session", "open", "--local", "--base", "HEAD~1", "--head", "HEAD"])
            .output()
            .unwrap(),
    )["session"];
    assert_eq!(target(session, "mode.txt")["status"], "modified");
    assert_eq!(target(session, "mode.txt")["hunks"].as_array().unwrap().len(), 1);
    let links: Vec<&serde_json::Value> = session["targets"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|t| t["path"] == "link")
        .collect();
    assert_eq!(links.len(), 1);
    assert_eq!(links[0]["status"], "type");
    assert_eq!(links[0]["hunks"].as_array().unwrap().len(), 2);
}

#[test]
fn nested_repository_is_not_an_untracked_target_and_clean_tree_is_not_dirty() {
    let state = tempfile::tempdir().unwrap();
    let repo = Repo::new();
    let clean = json(
        &reviewkit(&state, &repo)
            .args(["session", "open", "--local", "--base", "HEAD~1"])
            .output()
            .unwrap(),
    );
    assert_eq!(clean["session"]["head"]["dirty"], false, "nothing changed since HEAD");
    assert!(!clean["session"]["targets"].as_array().unwrap().is_empty());

    std::fs::create_dir(repo.path().join("nested")).unwrap();
    Process::new(real_git())
        .args(["init", "-q"])
        .current_dir(repo.path().join("nested"))
        .status()
        .unwrap();
    let session = &json(
        &reviewkit(&state, &repo)
            .args(["session", "open", "--local"])
            .output()
            .unwrap(),
    )["session"];
    assert!(
        session["targets"]
            .as_array()
            .unwrap()
            .iter()
            .all(|t| t["path"] != "nested/")
    );
}

#[test]
fn file_base_on_a_directory_is_exists_false() {
    let state = tempfile::tempdir().unwrap();
    let repo = Repo::new();
    std::fs::create_dir(repo.path().join("dir")).unwrap();
    repo.write("dir/f.txt", "x\n");
    repo.git(&["add", "-A"]);
    repo.git(&["commit", "-q", "-m", "dir"]);
    let opened = json(
        &reviewkit(&state, &repo)
            .args(["session", "open", "--local"])
            .output()
            .unwrap(),
    );
    let id = opened["session"]["id"].as_str().unwrap().to_string();
    let out = json(
        &reviewkit(&state, &repo)
            .args(["file", "base", "--session", &id, "--path", "dir"])
            .output()
            .unwrap(),
    );
    assert_eq!(out["data"]["exists"], false);
}

#[test]
fn bad_base_leaves_no_session_directory_behind() {
    let state = tempfile::tempdir().unwrap();
    let repo = Repo::new();
    let out = reviewkit(&state, &repo)
        .args(["session", "open", "--local", "--base", "nope"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(!state.path().join("sessions").exists());
}

#[test]
fn a_file_deleted_in_the_index_but_recreated_untracked_is_one_modified_target() {
    let state = tempfile::tempdir().unwrap();
    let repo = Repo::new();
    repo.git(&["rm", "-q", "--cached", "keep.txt"]);
    let session = &json(
        &reviewkit(&state, &repo)
            .args(["session", "open", "--local"])
            .output()
            .unwrap(),
    )["session"];
    let keeps: Vec<&serde_json::Value> = session["targets"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|t| t["path"] == "keep.txt")
        .collect();
    assert_eq!(keeps.len(), 1);
    assert_eq!(keeps[0]["status"], "modified");
    assert_eq!(keeps[0]["base_blob"].as_str().unwrap().len(), 40);
    assert_eq!(keeps[0]["commentable"]["base"], serde_json::json!([[1, 11]]));
}

#[test]
fn a_three_dot_base_compares_against_the_merge_base() {
    let state = tempfile::tempdir().unwrap();
    let repo = Repo::new();
    repo.git(&["checkout", "-q", "-b", "feature"]);
    repo.write("feature.txt", "f\n");
    repo.git(&["add", "feature.txt"]);
    repo.git(&["commit", "-q", "-m", "feature"]);
    repo.git(&["checkout", "-q", "main"]);
    repo.write("main-only.txt", "m\n");
    repo.git(&["add", "main-only.txt"]);
    repo.git(&["commit", "-q", "-m", "main moves on"]);
    repo.git(&["checkout", "-q", "feature"]);

    let paths = |body: &serde_json::Value| -> Vec<String> {
        body["session"]["targets"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["path"].as_str().unwrap().to_string())
            .collect()
    };
    let tip = json(
        &reviewkit(&state, &repo)
            .args(["session", "open", "--local", "--base", "main"])
            .output()
            .unwrap(),
    );
    assert!(paths(&tip).contains(&"main-only.txt".to_string()));

    let worktree = json(
        &reviewkit(&state, &repo)
            .args(["session", "open", "--local", "--base", "main..."])
            .output()
            .unwrap(),
    );
    assert_eq!(paths(&worktree), vec!["feature.txt"]);
    assert_eq!(worktree["session"]["source"]["base_spec"], "main...");

    let base_before = worktree["session"]["base"]["sha"].clone();
    repo.write("feature.txt", "f\nmore\n");
    repo.git(&["commit", "-q", "-am", "feature again"]);
    repo.git(&["merge", "-q", "--no-edit", "main"]);
    let reopened = json(
        &reviewkit(&state, &repo)
            .args(["session", "open", "--local", "--base", "main..."])
            .output()
            .unwrap(),
    );
    assert_ne!(reopened["session"]["base"]["sha"], base_before);
    assert_eq!(paths(&reopened), vec!["feature.txt"]);

    repo.git(&["checkout", "-q", "main"]);
    let range = json(
        &reviewkit(&state, &repo)
            .args(["session", "open", "--local", "--base", "main...feature"])
            .output()
            .unwrap(),
    );
    assert_eq!(paths(&range), vec!["feature.txt"]);
    assert_eq!(range["session"]["source"]["head_spec"], "feature");
}

#[test]
fn unrelated_histories_have_no_merge_base_and_leave_no_session() {
    let state = tempfile::tempdir().unwrap();
    let repo = Repo::new();
    repo.git(&["checkout", "-q", "--orphan", "other"]);
    repo.git(&["rm", "-rq", "--cached", "."]);
    repo.write("o.txt", "o\n");
    repo.git(&["add", "o.txt"]);
    repo.git(&["commit", "-q", "-m", "orphan"]);
    let out = reviewkit(&state, &repo)
        .args(["session", "open", "--local", "--base", "main..."])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let body: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(body["error"]["message"].as_str().unwrap().contains("no merge-base"));
    assert!(!state.path().join("sessions").exists());
}

#[test]
fn a_session_written_by_a_newer_cli_is_not_changed_and_exits_with_the_contract_code() {
    let state = tempfile::tempdir().unwrap();
    let repo = Repo::new();
    let opened = json(
        &reviewkit(&state, &repo)
            .args(["session", "open", "--local", "--base", "HEAD~1"])
            .output()
            .unwrap(),
    );
    let id = opened["session"]["id"].as_str().unwrap().to_string();
    let path = opened["session"]["state_path"].as_str().unwrap().to_string();
    let mut raw: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    raw["_writer"]["revision"] = serde_json::json!(999);
    std::fs::write(&path, serde_json::to_vec(&raw).unwrap()).unwrap();
    let before = std::fs::read(&path).unwrap();

    let out = reviewkit(&state, &repo)
        .args(["decision", "set", "--session", &id])
        .write_stdin(r#"{"event":"approve","body":""}"#)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(6));
    let body: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(body["error"]["details"]["direction"], "newer");
    assert_eq!(std::fs::read(&path).unwrap(), before);

    let got = json(
        &reviewkit(&state, &repo)
            .args(["session", "get", "--session", &id])
            .output()
            .unwrap(),
    );
    assert_eq!(got["session"]["id"], id.as_str());
}

#[test]
fn a_base_file_that_is_not_utf8_also_comes_as_raw_bytes() {
    let state = tempfile::tempdir().unwrap();
    let repo = Repo::new();
    std::fs::write(repo.path().join("latin1.txt"), b"caf\xe9\n").unwrap();
    repo.git(&["add", "latin1.txt"]);
    repo.git(&["commit", "-q", "-m", "latin1"]);
    std::fs::write(repo.path().join("latin1.txt"), b"caf\xe9!\n").unwrap();
    let opened = json(
        &reviewkit(&state, &repo)
            .args(["session", "open", "--local", "--base", "HEAD"])
            .output()
            .unwrap(),
    );
    let id = opened["session"]["id"].as_str().unwrap().to_string();
    let body = json(
        &reviewkit(&state, &repo)
            .args(["file", "base", "--session", &id, "--path", "latin1.txt"])
            .output()
            .unwrap(),
    );
    assert_eq!(body["data"]["raw"], "Y2Fm6Qo=");
    let utf8 = json(
        &reviewkit(&state, &repo)
            .args(["file", "base", "--session", &id, "--path", "keep.txt"])
            .output()
            .unwrap(),
    );
    assert!(utf8["data"].get("raw").is_none());
}

#[test]
fn a_worktree_refresh_hashes_all_changed_files_with_one_git_call() {
    let state = tempfile::tempdir().unwrap();
    let repo = Repo::new();
    for name in ["u1.txt", "u2.txt", "u3.txt"] {
        repo.write(name, "x\n");
    }
    repo.write("keep.txt", "changed\n");
    let bin = tempfile::tempdir().unwrap();
    let log = bin.path().join("calls");
    let wrapper = bin.path().join("git");
    std::fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\necho \"$*\" >> {}\nexec {} \"$@\"\n",
            log.display(),
            real_git().display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&wrapper, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let body = json(
        &reviewkit(&state, &repo)
            .env("REVIEWKIT_GIT", &wrapper)
            .args(["session", "open", "--local", "--base", "HEAD"])
            .output()
            .unwrap(),
    );
    let calls = std::fs::read_to_string(&log).unwrap();
    assert_eq!(
        calls.lines().filter(|l| l.contains("hash-object")).count(),
        1,
        "{calls}"
    );
    let targets = body["session"]["targets"].as_array().unwrap();
    assert!(
        targets
            .iter()
            .all(|t| t["status"] == "deleted" || t["head_blob"].is_string())
    );
}

#[test]
fn a_large_untracked_file_is_only_sniffed() {
    let state = tempfile::tempdir().unwrap();
    let repo = Repo::new();
    repo.write("big.log", &"line\n".repeat(400_000));
    let body = json(
        &reviewkit(&state, &repo)
            .args(["session", "open", "--local", "--base", "HEAD"])
            .output()
            .unwrap(),
    );
    let big = target(&body["session"], "big.log");
    assert_eq!(big["too_large"], true);
    assert_eq!(big["hunks"], serde_json::json!([]));
    assert_eq!(big["commentable"]["head"], serde_json::json!([]));
    assert!(big["head_blob"].is_string());
}
