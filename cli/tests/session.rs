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
    assert_eq!(
        listed["data"][0]["title"], "HEAD~1..worktree",
        "local ids are hashes, so the summary names the comparison"
    );

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
        .envs(common::ISOLATED_GIT)
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
fn a_small_user_rename_limit_does_not_turn_renames_into_deletes() {
    let state = tempfile::tempdir().unwrap();
    let repo = Repo::new();
    let lines = |name: &str| (1..=50).map(|i| format!("{name} line {i}\n")).collect::<String>();
    repo.write("a.txt", &lines("a"));
    repo.write("b.txt", &lines("b"));
    repo.git(&["add", "-A"]);
    repo.git(&["commit", "-q", "-m", "base"]);
    for name in ["a", "b"] {
        repo.git(&["mv", &format!("{name}.txt"), &format!("{name}-moved.txt")]);
        repo.write(&format!("{name}-moved.txt"), &(lines(name) + "extra\n"));
    }
    repo.git(&["config", "diff.renameLimit", "1"]);
    let session = &json(
        &reviewkit(&state, &repo)
            .args(["session", "open", "--local", "--base", "HEAD"])
            .output()
            .unwrap(),
    )["session"];
    for name in ["a", "b"] {
        let moved = target(session, &format!("{name}-moved.txt"));
        assert_eq!(moved["status"], "renamed", "{moved}");
        assert_eq!(moved["old_path"], format!("{name}.txt"));
        assert_eq!(moved["stats"]["added"], 1, "the patch pairs the rename too: {moved}");
    }
}

#[test]
fn user_diff_algorithm_does_not_change_the_hunks() {
    let state = tempfile::tempdir().unwrap();
    let repo = Repo::new();
    // f.rs splits differently under histogram, g.rs without the indent
    // heuristic.
    repo.write("f.rs", "{\n    z\n}\n");
    repo.write("g.rs", "\n{\ny\ny\n{\n\n");
    repo.git(&["add", "-A"]);
    repo.git(&["commit", "-q", "-m", "base"]);
    repo.write("f.rs", "{\n{\n}\n    z\ny\n}\n");
    repo.write("g.rs", "\n{\n    z\n{\ny\ny\n{\n{\nx\n{\n\n");
    let opened = json(
        &reviewkit(&state, &repo)
            .args(["session", "open", "--local"])
            .output()
            .unwrap(),
    );
    let hunks = |session: &serde_json::Value| {
        session["targets"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["hunks"].clone())
            .collect::<Vec<_>>()
    };
    let myers = hunks(&opened["session"]);
    repo.git(&["config", "diff.algorithm", "histogram"]);
    repo.git(&["config", "diff.indentHeuristic", "false"]);
    let refreshed = json(
        &reviewkit(&state, &repo)
            .args([
                "session",
                "refresh",
                "--session",
                opened["session"]["id"].as_str().unwrap(),
            ])
            .output()
            .unwrap(),
    );
    let configured = hunks(&refreshed["session"]);
    assert_eq!(myers.len(), 2);
    assert_eq!(configured, myers);
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
        .envs(common::ISOLATED_GIT)
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
fn file_head_reads_the_head_commit_not_the_working_tree() {
    let state = tempfile::tempdir().unwrap();
    let repo = Repo::new();
    repo.write("keep.txt", "edited in the working tree\n");
    let opened = json(
        &reviewkit(&state, &repo)
            .args(["session", "open", "--local", "--base", "HEAD~1", "--head", "HEAD"])
            .output()
            .unwrap(),
    );
    let id = opened["session"]["id"].as_str().unwrap().to_string();
    let head = json(
        &reviewkit(&state, &repo)
            .args(["file", "head", "--session", &id, "--path", "keep.txt"])
            .output()
            .unwrap(),
    );
    assert_eq!(head["data"]["exists"], true);
    let lines = head["data"]["lines"].as_array().unwrap();
    assert!(!lines.iter().any(|l| l == "edited in the working tree"), "{lines:?}");

    let worktree = json(
        &reviewkit(&state, &repo)
            .args(["session", "open", "--local", "--base", "HEAD~1"])
            .output()
            .unwrap(),
    );
    let wid = worktree["session"]["id"].as_str().unwrap().to_string();
    let out = reviewkit(&state, &repo)
        .args(["file", "head", "--session", &wid, "--path", "keep.txt"])
        .output()
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        body["error"]["code"], "E_INVALID",
        "the working tree is the head side: {body}"
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

#[test]
fn a_large_file_with_a_small_change_is_too_large_on_both_sides_alike() {
    let state = tempfile::tempdir().unwrap();
    let repo = Repo::new();
    let big = "line\n".repeat(300_000);
    repo.write("lock.json", &big);
    repo.git(&["add", "lock.json"]);
    repo.git(&["commit", "-q", "-m", "big file"]);
    repo.write("lock.json", &format!("{big}one more\n"));
    let body = json(
        &reviewkit(&state, &repo)
            .args(["session", "open", "--local", "--base", "HEAD"])
            .output()
            .unwrap(),
    );
    let id = body["session"]["id"].as_str().unwrap().to_string();
    let lock = target(&body["session"], "lock.json");
    assert_eq!(lock["too_large"], true, "{lock}");
    assert_eq!(lock["hunks"], serde_json::json!([]));
    let base = json(
        &reviewkit(&state, &repo)
            .args(["file", "base", "--session", &id, "--path", "lock.json"])
            .output()
            .unwrap(),
    );
    assert_eq!(base["data"]["too_large"], true);
    assert!(base["data"]["lines"].is_null());
}

#[test]
fn a_too_large_file_is_not_commentable_on_github_and_is_not_read_for_anchors() {
    let state = tempfile::tempdir().unwrap();
    let repo = Repo::new();
    let big = "line\n".repeat(300_000);
    repo.write("lock.json", &big);
    repo.git(&["add", "lock.json"]);
    repo.git(&["commit", "-q", "-m", "big file"]);
    repo.write("lock.json", &format!("{big}one more\n"));
    let body = json(
        &reviewkit(&state, &repo)
            .args(["session", "open", "--local", "--base", "HEAD"])
            .output()
            .unwrap(),
    );
    let id = body["session"]["id"].as_str().unwrap().to_string();
    let lock = target(&body["session"], "lock.json");
    assert_eq!(lock["commentable"]["head"], serde_json::json!([]), "{lock}");
    assert_eq!(lock["commentable"]["base"], serde_json::json!([]), "{lock}");
    for (side, line) in [("head", 300_001), ("base", 300_000)] {
        let added = json(
            &reviewkit(&state, &repo)
                .args(["comment", "add", "--session", &id])
                .write_stdin(format!(
                    r#"{{"path": "lock.json", "side": "{side}", "line": {line}, "body": "x"}}"#
                ))
                .output()
                .unwrap(),
        );
        let comment = added["session"]["comments"].as_array().unwrap().last().unwrap().clone();
        assert_eq!(comment["commentable"]["github"], false, "{side}: {comment}");
        assert_eq!(comment["commentable"]["reason"], "too_large", "{side}");
        assert_eq!(comment["anchor"]["context"]["lines"], serde_json::json!([]), "{side}");
    }
    let refreshed = json(
        &reviewkit(&state, &repo)
            .args(["session", "refresh", "--session", &id])
            .output()
            .unwrap(),
    );
    for comment in refreshed["session"]["comments"].as_array().unwrap() {
        assert_eq!(comment["stale"], false, "{comment}");
        assert_eq!(comment["commentable"]["github"], false, "{comment}");
    }
}

#[test]
fn a_comment_on_a_large_working_tree_file_pins_the_file_as_it_is_now() {
    let state = tempfile::tempdir().unwrap();
    let repo = Repo::new();
    let big = "line\n".repeat(300_000);
    repo.write("lock.json", &big);
    repo.git(&["add", "lock.json"]);
    repo.git(&["commit", "-q", "-m", "big file"]);
    repo.write("lock.json", &format!("{big}one more\n"));
    let body = json(
        &reviewkit(&state, &repo)
            .args(["session", "open", "--local", "--base", "HEAD"])
            .output()
            .unwrap(),
    );
    let id = body["session"]["id"].as_str().unwrap().to_string();
    repo.write("lock.json", &format!("{big}changed since the refresh\n"));
    let now = Process::new(real_git())
        .current_dir(repo.path())
        .args(["hash-object", "lock.json"])
        .output()
        .unwrap();
    let now = String::from_utf8(now.stdout).unwrap().trim().to_string();
    let added = json(
        &reviewkit(&state, &repo)
            .args(["comment", "add", "--session", &id])
            .write_stdin(r#"{"path": "lock.json", "side": "head", "line": 1, "body": "x"}"#)
            .output()
            .unwrap(),
    );
    let comment = added["session"]["comments"].as_array().unwrap().last().unwrap().clone();
    assert_eq!(comment["anchor"]["blob"], now.as_str(), "{comment}");
}

#[test]
fn a_large_file_whose_name_looks_like_pathspec_magic_is_still_sized() {
    let state = tempfile::tempdir().unwrap();
    let repo = Repo::new();
    let big = "line\n".repeat(300_000);
    repo.write(":big", &big);
    repo.git(&["add", "--", ":(literal):big"]);
    repo.git(&["commit", "-q", "-m", "big file"]);
    repo.git(&["checkout", "-q", "-b", "next"]);
    repo.write(":big", &format!("{big}one more\n"));
    repo.git(&["commit", "-q", "-am", "one line"]);
    let body = json(
        &reviewkit(&state, &repo)
            .args(["session", "open", "--local", "--base", "HEAD~1", "--head", "HEAD"])
            .output()
            .unwrap(),
    );
    let big = target(&body["session"], ":big");
    assert_eq!(big["too_large"], true, "{big}");
    assert_eq!(big["hunks"], serde_json::json!([]));
}

fn logging_git(bin: &tempfile::TempDir, line: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let log = bin.path().join("calls");
    let wrapper = bin.path().join("git");
    std::fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\necho \"{line}\" >> {}\nexec {} \"$@\"\n",
            log.display(),
            real_git().display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&wrapper, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    (wrapper, log)
}

#[test]
fn a_git_dir_inherited_from_the_caller_does_not_redirect_git() {
    let state = tempfile::tempdir().unwrap();
    let repo = Repo::new();
    let other = Repo::new();
    other.write("elsewhere.txt", "x\n");
    let body = json(
        &reviewkit(&state, &repo)
            .env("GIT_DIR", other.path().join(".git"))
            .env("GIT_WORK_TREE", other.path())
            .args(["session", "open", "--local", "--base", "HEAD"])
            .output()
            .unwrap(),
    );
    assert_eq!(
        std::path::Path::new(body["session"]["repo"]["root"].as_str().unwrap())
            .canonicalize()
            .unwrap(),
        repo.path().canonicalize().unwrap()
    );
    let paths: Vec<&str> = body["session"]["targets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["path"].as_str().unwrap())
        .collect();
    assert!(!paths.contains(&"elsewhere.txt"), "{paths:?}");
}

#[test]
fn git_runs_without_optional_locks_askpass_or_a_terminal() {
    let state = tempfile::tempdir().unwrap();
    let repo = Repo::new();
    repo.write("keep.txt", "changed\n");
    let bin = tempfile::tempdir().unwrap();
    let (wrapper, log) = logging_git(
        &bin,
        "locks=$GIT_OPTIONAL_LOCKS askpass=[$GIT_ASKPASS$SSH_ASKPASS] require=$SSH_ASKPASS_REQUIRE index=[$GIT_INDEX_FILE] pid=$$ pgid=$(ps -o pgid= -p $$ | tr -d ' ') tty=$(ps -o tty= -p $$ | tr -d ' ')",
    );
    json(
        &reviewkit(&state, &repo)
            .env("REVIEWKIT_GIT", &wrapper)
            .env("GIT_ASKPASS", "/bin/false")
            .env("SSH_ASKPASS", "/bin/false")
            .env("GIT_INDEX_FILE", "/nonexistent/index")
            .args(["session", "open", "--local", "--base", "HEAD"])
            .output()
            .unwrap(),
    );
    let calls = std::fs::read_to_string(&log).unwrap();
    assert!(calls.lines().count() > 0);
    for line in calls.lines() {
        assert!(line.starts_with("locks=0 askpass=[] require=never index=[] "), "{line}");
        assert!(
            line.ends_with("tty=??") || line.ends_with("tty=?"),
            "no terminal: {line}"
        );
        let field = |name: &str| {
            line.split_whitespace()
                .find_map(|w| w.strip_prefix(&format!("{name}=")))
                .unwrap()
                .to_string()
        };
        assert_eq!(field("pgid"), field("pid"), "a session of its own: {line}");
    }
}

#[test]
fn an_untracked_fifo_or_symlink_to_a_device_does_not_hang_a_refresh() {
    let state = tempfile::tempdir().unwrap();
    let repo = Repo::new();
    let fifo = repo.path().join("pipe");
    assert!(
        std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap()
            .success()
    );
    std::os::unix::fs::symlink("/dev/zero", repo.path().join("zero")).unwrap();
    let mut cmd = reviewkit(&state, &repo);
    cmd.timeout(std::time::Duration::from_secs(30));
    let body = json(
        &cmd.args(["session", "open", "--local", "--base", "HEAD"])
            .output()
            .unwrap(),
    );
    let paths: Vec<&str> = body["session"]["targets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["path"].as_str().unwrap())
        .collect();
    assert!(!paths.contains(&"pipe"), "{paths:?}");
    assert!(!paths.contains(&"zero"), "{paths:?}");
}

#[test]
fn an_untracked_symlink_to_a_file_reads_as_that_file_everywhere() {
    let state = tempfile::tempdir().unwrap();
    let repo = Repo::new();
    repo.write("real.txt", "a\nb\nc\nd\n");
    std::os::unix::fs::symlink("real.txt", repo.path().join("link")).unwrap();
    let body = json(
        &reviewkit(&state, &repo)
            .args(["session", "open", "--local", "--base", "HEAD"])
            .output()
            .unwrap(),
    );
    let id = body["session"]["id"].as_str().unwrap().to_string();
    let link = target(&body["session"], "link");
    assert_eq!(link["stats"]["added"], 4, "{link}");
    assert!(
        link["head_blob"].is_string(),
        "hashed through the link, so viewed resets on a change: {link}"
    );
    let added = json(
        &reviewkit(&state, &repo)
            .args(["comment", "add", "--session", &id])
            .write_stdin(r#"{"path": "link", "side": "head", "line": 3, "body": "x"}"#)
            .output()
            .unwrap(),
    );
    assert_eq!(
        added["session"]["comments"][0]["anchor"]["context"]["lines"],
        serde_json::json!(["c"])
    );
}

#[test]
fn a_two_dot_base_with_a_head_as_well_is_refused() {
    let state = tempfile::tempdir().unwrap();
    let repo = Repo::new();
    let out = reviewkit(&state, &repo)
        .args(["session", "open", "--local", "--base", "HEAD~1..HEAD", "--head", "HEAD"])
        .output()
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(body["error"]["code"], "E_INVALID");
}

#[test]
fn a_large_file_deleted_or_only_large_at_the_head_commit_is_too_large() {
    let state = tempfile::tempdir().unwrap();
    let repo = Repo::new();
    let big = "line\n".repeat(300_000);
    repo.write("big.txt", &big);
    repo.git(&["add", "big.txt"]);
    repo.git(&["commit", "-q", "-m", "big"]);
    std::fs::remove_file(repo.path().join("big.txt")).unwrap();
    let deleted = json(
        &reviewkit(&state, &repo)
            .args(["session", "open", "--local", "--base", "HEAD"])
            .output()
            .unwrap(),
    );
    assert_eq!(
        target(&deleted["session"], "big.txt")["too_large"],
        true,
        "a large deleted base"
    );

    let state = tempfile::tempdir().unwrap();
    let commits = json(
        &reviewkit(&state, &repo)
            .args(["session", "open", "--local", "--base", "HEAD~1", "--head", "HEAD"])
            .output()
            .unwrap(),
    );
    let id = commits["session"]["id"].as_str().unwrap().to_string();
    assert_eq!(target(&commits["session"], "big.txt")["too_large"], true);
    let head = json(
        &reviewkit(&state, &repo)
            .args(["file", "head", "--session", &id, "--path", "big.txt"])
            .output()
            .unwrap(),
    );
    assert_eq!(head["data"]["too_large"], true);
    assert!(head["data"]["lines"].is_null());
}

#[test]
fn a_git_command_that_hangs_in_a_partial_clone_times_out() {
    for promisor in ["extensions.partialClone=origin", "remote.origin.promisor=true"] {
        let (key, value) = promisor.split_once('=').unwrap();
        let state = tempfile::tempdir().unwrap();
        let repo = Repo::new();
        repo.git(&["config", key, value]);
        repo.write("keep.txt", "changed\n");
        let bin = tempfile::tempdir().unwrap();
        let slow_git = bin.path().join("git");
        // A partial clone fetches missing blobs inside commands like diff, with
        // no deadline of their own.
        std::fs::write(
            &slow_git,
            format!(
                "#!/bin/sh\ncase \"$*\" in *\" diff \"*) sleep 30 ;; esac\nexec {} \"$@\"\n",
                real_git().display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&slow_git, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        let started = std::time::Instant::now();
        let out = reviewkit(&state, &repo)
            .env("REVIEWKIT_GIT", &slow_git)
            .env("REVIEWKIT_GIT_TIMEOUT_MS", "500")
            .args(["session", "open", "--local", "--base", "HEAD"])
            .output()
            .unwrap();
        assert!(started.elapsed() < std::time::Duration::from_secs(20));
        let body: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(body["error"]["code"], "E_GIT");
        assert!(body["error"]["message"].as_str().unwrap().contains("did not finish"));
    }
}

#[test]
fn a_moved_checkout_is_reported_as_missing_not_as_a_missing_git() {
    let state = tempfile::tempdir().unwrap();
    let repo = Repo::new();
    repo.write("keep.txt", "changed\n");
    let opened = json(
        &reviewkit(&state, &repo)
            .args(["session", "open", "--local", "--base", "HEAD"])
            .output()
            .unwrap(),
    );
    let moved = repo.path().with_extension("moved");
    std::fs::rename(repo.path(), &moved).unwrap();
    let out = Command::cargo_bin("reviewkit")
        .unwrap()
        .envs(common::ISOLATED_GIT)
        .env("REVIEWKIT_GIT", real_git())
        .arg("--state-dir")
        .arg(state.path())
        .args([
            "session",
            "refresh",
            "--session",
            opened["session"]["id"].as_str().unwrap(),
        ])
        .output()
        .unwrap();
    std::fs::rename(&moved, repo.path()).unwrap();
    let body: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(body["error"]["code"], "E_GIT", "{body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("checkout not found"),
        "{body}"
    );
}

#[test]
fn reading_or_commenting_on_a_file_of_a_moved_checkout_names_the_checkout() {
    let state = tempfile::tempdir().unwrap();
    let repo = Repo::new();
    repo.write("keep.txt", "changed\n");
    let opened = json(
        &reviewkit(&state, &repo)
            .args(["session", "open", "--local", "--base", "HEAD"])
            .output()
            .unwrap(),
    );
    let id = opened["session"]["id"].as_str().unwrap().to_string();
    let moved = repo.path().with_extension("moved");
    std::fs::rename(repo.path(), &moved).unwrap();
    let run = |args: &[&str], stdin: &str| {
        let out = Command::cargo_bin("reviewkit")
            .unwrap()
            .envs(common::ISOLATED_GIT)
            .env("REVIEWKIT_GIT", real_git())
            .arg("--state-dir")
            .arg(state.path())
            .args(args)
            .write_stdin(stdin)
            .output()
            .unwrap();
        serde_json::from_slice::<serde_json::Value>(&out.stdout).unwrap()
    };
    let base = run(&["file", "base", "--session", &id, "--path", "keep.txt"], "");
    let added = run(
        &["comment", "add", "--session", &id],
        r#"{"path": "keep.txt", "side": "head", "line": 1, "body": "x"}"#,
    );
    std::fs::rename(&moved, repo.path()).unwrap();
    for body in [base, added] {
        assert_eq!(body["error"]["code"], "E_GIT", "{body}");
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .contains("checkout not found"),
            "{body}"
        );
    }
}

#[test]
fn a_session_whose_checkout_moved_is_listed_as_missing() {
    let state = tempfile::tempdir().unwrap();
    let repo = Repo::new();
    repo.write("keep.txt", "changed\n");
    json(
        &reviewkit(&state, &repo)
            .args(["session", "open", "--local", "--base", "HEAD"])
            .output()
            .unwrap(),
    );
    let list = || {
        json(
            &Command::cargo_bin("reviewkit")
                .unwrap()
                .envs(common::ISOLATED_GIT)
                .env("REVIEWKIT_GIT", real_git())
                .arg("--state-dir")
                .arg(state.path())
                .args(["session", "list"])
                .output()
                .unwrap(),
        )
    };
    assert_eq!(list()["data"][0]["missing"], false);
    let moved = repo.path().with_extension("moved");
    std::fs::rename(repo.path(), &moved).unwrap();
    let listed = list();
    std::fs::rename(&moved, repo.path()).unwrap();
    assert_eq!(listed["data"][0]["missing"], true, "{listed}");
}
