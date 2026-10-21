mod common;

use common::{Repo, json, reviewkit};

fn open(state: &tempfile::TempDir, repo: &Repo) -> String {
    let opened = json(
        &reviewkit(state, repo)
            .args(["session", "open", "--local", "--base", "HEAD~1"])
            .output()
            .unwrap(),
    );
    opened["session"]["id"].as_str().unwrap().to_string()
}

fn add(state: &tempfile::TempDir, repo: &Repo, id: &str, input: serde_json::Value) -> std::process::Output {
    reviewkit(state, repo)
        .args(["comment", "add", "--session", id])
        .write_stdin(input.to_string())
        .output()
        .unwrap()
}

#[test]
fn add_fills_id_anchor_and_commentability() {
    let state = tempfile::tempdir().unwrap();
    let repo = Repo::new();
    let id = open(&state, &repo);
    let body = json(&add(
        &state,
        &repo,
        &id,
        serde_json::json!({"path": "keep.txt", "side": "head", "line": 3, "body": "why C?"}),
    ));
    let cid = body["data"]["id"].as_str().unwrap();
    assert!(cid.starts_with("c_"));
    assert_eq!(body["session"]["rev"], 2);
    let comment = &body["session"]["comments"][0];
    assert_eq!(comment["id"], cid);
    assert_eq!(comment["kind"], "issue");
    assert_eq!(
        comment["commentable"],
        serde_json::json!({"github": true, "reason": null})
    );
    assert_eq!(comment["anchor"]["context"]["lines"], serde_json::json!(["C"]));
    assert_eq!(comment["anchor"]["context"]["before"], serde_json::json!(["a", "b"]));
    assert!(
        comment["anchor"]["head_sha"].is_null(),
        "worktree sessions pin by blob, not commit"
    );
    assert_eq!(comment["anchor"]["base_sha"].as_str().unwrap().len(), 40);
    assert_eq!(comment["anchor"]["blob"].as_str().unwrap().len(), 40);
    assert_eq!(comment["stale"], false);
}

#[test]
fn outside_the_diff_is_queued_but_not_commentable_on_github() {
    let state = tempfile::tempdir().unwrap();
    let repo = Repo::new();
    let id = open(&state, &repo);
    let body = json(&add(
        &state,
        &repo,
        &id,
        serde_json::json!({"path": "keep.txt", "side": "head", "line": 7, "body": "far"}),
    ));
    assert_eq!(
        body["session"]["comments"][0]["commentable"],
        serde_json::json!({"github": false, "reason": "outside_diff"})
    );
    let rejected = add(
        &state,
        &repo,
        &id,
        serde_json::json!({"path": "added.txt", "side": "base", "line": 1, "body": "?"}),
    );
    assert_eq!(rejected.status.code(), Some(2), "added.txt does not exist on base");
    let listed = json(
        &reviewkit(&state, &repo)
            .args(["comment", "list", "--session", &id])
            .output()
            .unwrap(),
    );
    assert_eq!(listed["data"].as_array().unwrap().len(), 1);
}

#[test]
fn impossible_lines_and_missing_paths_are_e_not_commentable() {
    let state = tempfile::tempdir().unwrap();
    let repo = Repo::new();
    let id = open(&state, &repo);
    let out = add(
        &state,
        &repo,
        &id,
        serde_json::json!({"path": "keep.txt", "side": "head", "line": 99, "body": "x"}),
    );
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&out.stdout).unwrap()["error"]["code"],
        "E_NOT_COMMENTABLE"
    );
    let out = add(
        &state,
        &repo,
        &id,
        serde_json::json!({"path": "nope.txt", "side": "head", "line": 1, "body": "x"}),
    );
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&out.stdout).unwrap()["error"]["code"],
        "E_NOT_COMMENTABLE"
    );
    let out = add(
        &state,
        &repo,
        &id,
        serde_json::json!({"path": "gone.txt", "side": "head", "line": 1, "body": "deleted on head"}),
    );
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&out.stdout).unwrap()["error"]["code"],
        "E_NOT_COMMENTABLE"
    );
}

#[test]
fn bad_stdin_is_e_invalid() {
    let state = tempfile::tempdir().unwrap();
    let repo = Repo::new();
    let id = open(&state, &repo);
    let out = reviewkit(&state, &repo)
        .args(["comment", "add", "--session", &id])
        .write_stdin("{ nope")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&out.stdout).unwrap()["error"]["code"],
        "E_INVALID"
    );
    let out = reviewkit(&state, &repo)
        .args(["comment", "add", "--session", &id])
        .write_stdin("")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn update_moves_the_anchor_and_clears_a_suggestion_with_null() {
    let state = tempfile::tempdir().unwrap();
    let repo = Repo::new();
    let id = open(&state, &repo);
    let added = json(&add(
        &state,
        &repo,
        &id,
        serde_json::json!({"path": "keep.txt", "side": "head", "line": 3, "body": "b", "suggestion": "C!"}),
    ));
    let cid = added["data"]["id"].as_str().unwrap().to_string();
    let before = added["session"]["etags"].clone();

    let updated = json(
        &reviewkit(&state, &repo)
            .args(["comment", "update", "--session", &id, "--id", &cid])
            .write_stdin(r#"{"line": 7, "suggestion": null, "kind": "nit"}"#)
            .output()
            .unwrap(),
    );
    let comment = &updated["session"]["comments"][0];
    assert_eq!(comment["line"], 7);
    assert_eq!(comment["kind"], "nit");
    assert_eq!(comment["suggestion"], serde_json::Value::Null);
    assert_eq!(comment["body"], "b", "omitted fields stay");
    assert_eq!(comment["anchor"]["context"]["lines"], serde_json::json!(["g"]));
    assert_eq!(
        comment["commentable"]["github"], false,
        "line 7 is outside the diff now"
    );
    assert_ne!(updated["session"]["etags"]["comments"], before["comments"]);
    assert_eq!(updated["session"]["etags"]["targets"], before["targets"]);
}

#[test]
fn remove_and_list_by_path() {
    let state = tempfile::tempdir().unwrap();
    let repo = Repo::new();
    let id = open(&state, &repo);
    let a = json(&add(
        &state,
        &repo,
        &id,
        serde_json::json!({"path": "keep.txt", "side": "head", "line": 1, "body": "a"}),
    ));
    let b = json(&add(
        &state,
        &repo,
        &id,
        serde_json::json!({"path": "added.txt", "side": "head", "line": 1, "body": "b"}),
    ));
    let listed = json(
        &reviewkit(&state, &repo)
            .args(["comment", "list", "--session", &id, "--path", "added.txt"])
            .output()
            .unwrap(),
    );
    assert_eq!(listed["data"].as_array().unwrap().len(), 1);
    assert_eq!(listed["data"][0]["id"], b["data"]["id"]);

    let removed = json(
        &reviewkit(&state, &repo)
            .args([
                "comment",
                "remove",
                "--session",
                &id,
                "--id",
                a["data"]["id"].as_str().unwrap(),
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(removed["session"]["comments"].as_array().unwrap().len(), 1);
    let gone = reviewkit(&state, &repo)
        .args(["comment", "remove", "--session", &id, "--id", "c_nope"])
        .output()
        .unwrap();
    assert_eq!(gone.status.code(), Some(3));
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&gone.stdout).unwrap()["error"]["code"],
        "E_NO_COMMENT"
    );
}

#[test]
fn decision_set_and_clear_touch_only_the_decision_etag() {
    let state = tempfile::tempdir().unwrap();
    let repo = Repo::new();
    let id = open(&state, &repo);
    let set = json(
        &reviewkit(&state, &repo)
            .args(["decision", "set", "--session", &id])
            .write_stdin(r#"{"event": "request_changes", "body": "please"}"#)
            .output()
            .unwrap(),
    );
    assert_eq!(set["session"]["decision"]["event"], "request_changes");
    assert!(!set["session"]["decision"]["updated_at"].as_str().unwrap().is_empty());
    let cleared = json(
        &reviewkit(&state, &repo)
            .args(["decision", "clear", "--session", &id])
            .output()
            .unwrap(),
    );
    assert_eq!(cleared["session"]["decision"], serde_json::Value::Null);
    assert_ne!(
        cleared["session"]["etags"]["decision"],
        set["session"]["etags"]["decision"]
    );
    assert_eq!(
        cleared["session"]["etags"]["comments"],
        set["session"]["etags"]["comments"]
    );
}

#[test]
fn viewed_follows_the_head_blob() {
    let state = tempfile::tempdir().unwrap();
    let repo = Repo::new();
    let id = json(
        &reviewkit(&state, &repo)
            .args(["session", "open", "--local"])
            .output()
            .unwrap(),
    )["session"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    repo.write("keep.txt", "edited\n");
    let opened = json(
        &reviewkit(&state, &repo)
            .args(["session", "open", "--session", &id])
            .output()
            .unwrap(),
    );
    assert_eq!(opened["session"]["targets"][0]["viewed"], false);

    let marked = json(
        &reviewkit(&state, &repo)
            .args(["target", "viewed", "--session", &id, "--path", "keep.txt"])
            .output()
            .unwrap(),
    );
    assert_eq!(marked["session"]["targets"][0]["viewed"], true);
    assert_eq!(
        marked["session"]["viewed"]["keep.txt"]["blob"].as_str().unwrap().len(),
        40
    );

    repo.write("keep.txt", "edited again\n");
    let refreshed = json(
        &reviewkit(&state, &repo)
            .args(["session", "open", "--session", &id])
            .output()
            .unwrap(),
    );
    assert_eq!(
        refreshed["session"]["targets"][0]["viewed"], false,
        "the file changed since it was viewed"
    );

    let undone = json(
        &reviewkit(&state, &repo)
            .args(["target", "viewed", "--session", &id, "--path", "keep.txt", "--undo"])
            .output()
            .unwrap(),
    );
    assert!(undone["session"]["viewed"].as_object().unwrap().is_empty());
    let missing = reviewkit(&state, &repo)
        .args(["target", "viewed", "--session", &id, "--path", "nope"])
        .output()
        .unwrap();
    assert_eq!(missing.status.code(), Some(3));
}

#[test]
fn base_side_comments_use_the_head_path_even_for_renames_and_deletions() {
    let state = tempfile::tempdir().unwrap();
    let repo = Repo::new();
    let id = open(&state, &repo);
    let modified = json(&add(
        &state,
        &repo,
        &id,
        serde_json::json!({"path": "keep.txt", "side": "base", "line": 3, "body": "old c"}),
    ));
    let c = &modified["session"]["comments"][0];
    assert_eq!(c["anchor"]["context"]["lines"], serde_json::json!(["c"]));
    assert_eq!(c["commentable"]["github"], true);

    let renamed = json(&add(
        &state,
        &repo,
        &id,
        serde_json::json!({"path": "new.txt", "side": "base", "line": 2, "body": "was old.txt"}),
    ));
    let c = &renamed["session"]["comments"][1];
    assert_eq!(c["path"], "new.txt");
    assert_eq!(c["anchor"]["context"]["lines"], serde_json::json!(["two"]));
    assert_eq!(c["anchor"]["blob"].as_str().unwrap().len(), 40);

    let deleted = json(&add(
        &state,
        &repo,
        &id,
        serde_json::json!({"path": "gone.txt", "side": "base", "line": 1, "body": "why delete?"}),
    ));
    let c = &deleted["session"]["comments"][2];
    assert_eq!(c["anchor"]["context"]["lines"], serde_json::json!(["gone"]));
    assert_eq!(c["commentable"]["github"], true);

    let base_of_renamed = json(
        &reviewkit(&state, &repo)
            .args(["file", "base", "--session", &id, "--path", "new.txt"])
            .output()
            .unwrap(),
    );
    assert_eq!(
        base_of_renamed["data"]["lines"],
        serde_json::json!(["one", "two", "three"])
    );
}

#[test]
fn committed_head_pins_head_sha_and_ranges_use_start_line() {
    let state = tempfile::tempdir().unwrap();
    let repo = Repo::new();
    let opened = json(
        &reviewkit(&state, &repo)
            .args(["session", "open", "--local", "--base", "HEAD~1", "--head", "HEAD"])
            .output()
            .unwrap(),
    );
    let id = opened["session"]["id"].as_str().unwrap().to_string();
    let body = json(&add(
        &state,
        &repo,
        &id,
        serde_json::json!({"path": "keep.txt", "side": "head", "line": 4, "start_line": 2, "body": "range"}),
    ));
    let c = &body["session"]["comments"][0];
    assert_eq!(c["anchor"]["head_sha"].as_str().unwrap().len(), 40);
    assert_eq!(c["anchor"]["context"]["lines"], serde_json::json!(["b", "C", "d"]));
    assert_eq!(c["anchor"]["context"]["before"], serde_json::json!(["a"]));
    assert_eq!(c["commentable"]["github"], true);
    let bad = add(
        &state,
        &repo,
        &id,
        serde_json::json!({"path": "keep.txt", "side": "head", "line": 2, "start_line": 4, "body": "x"}),
    );
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&bad.stdout).unwrap()["error"]["code"],
        "E_INVALID"
    );
}

#[test]
fn update_of_start_line_only_reanchors_and_a_vanished_path_blocks_moves_only() {
    let state = tempfile::tempdir().unwrap();
    let repo = Repo::new();
    let id = open(&state, &repo);
    let added = json(&add(
        &state,
        &repo,
        &id,
        serde_json::json!({"path": "keep.txt", "side": "head", "line": 4, "body": "b"}),
    ));
    let cid = added["data"]["id"].as_str().unwrap().to_string();
    let widened = json(
        &reviewkit(&state, &repo)
            .args(["comment", "update", "--session", &id, "--id", &cid])
            .write_stdin(r#"{"start_line": 2}"#)
            .output()
            .unwrap(),
    );
    assert_eq!(
        widened["session"]["comments"][0]["anchor"]["context"]["lines"],
        serde_json::json!(["b", "C", "d"])
    );

    std::fs::remove_file(repo.path().join("keep.txt")).unwrap();
    let body_only = reviewkit(&state, &repo)
        .args(["comment", "update", "--session", &id, "--id", &cid])
        .write_stdin(r#"{"body": "still fine"}"#)
        .output()
        .unwrap();
    assert!(body_only.status.success());
    let moved = reviewkit(&state, &repo)
        .args(["comment", "update", "--session", &id, "--id", &cid])
        .write_stdin(r#"{"line": 1}"#)
        .output()
        .unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&moved.stdout).unwrap()["error"]["code"],
        "E_NOT_COMMENTABLE"
    );
}

#[test]
fn binary_and_untracked_and_escaping_paths() {
    let state = tempfile::tempdir().unwrap();
    let repo = Repo::new();
    std::fs::write(repo.path().join("blob.bin"), b"\x00\x01\x02").unwrap();
    repo.write("scratch.txt", "s1\ns2\n");
    let id = json(
        &reviewkit(&state, &repo)
            .args(["session", "open", "--local"])
            .output()
            .unwrap(),
    )["session"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let bin = json(&add(
        &state,
        &repo,
        &id,
        serde_json::json!({"path": "blob.bin", "side": "head", "line": 1, "body": "what is this?"}),
    ));
    assert_eq!(
        bin["session"]["comments"][0]["commentable"],
        serde_json::json!({"github": false, "reason": "binary"})
    );
    let untracked = json(&add(
        &state,
        &repo,
        &id,
        serde_json::json!({"path": "scratch.txt", "side": "head", "line": 2, "body": "new"}),
    ));
    assert_eq!(untracked["session"]["comments"][1]["commentable"]["github"], true);
    let escaping = add(
        &state,
        &repo,
        &id,
        serde_json::json!({"path": "../etc/passwd", "side": "head", "line": 1, "body": "x"}),
    );
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&escaping.stdout).unwrap()["error"]["code"],
        "E_INVALID"
    );
}

#[test]
fn viewed_works_for_a_deleted_target() {
    let state = tempfile::tempdir().unwrap();
    let repo = Repo::new();
    let id = open(&state, &repo);
    let marked = json(
        &reviewkit(&state, &repo)
            .args(["target", "viewed", "--session", &id, "--path", "gone.txt"])
            .output()
            .unwrap(),
    );
    let gone = marked["session"]["targets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["path"] == "gone.txt")
        .unwrap();
    assert_eq!(gone["viewed"], true);
}

#[test]
fn the_pre_rename_name_is_rejected_with_a_hint() {
    let state = tempfile::tempdir().unwrap();
    let repo = Repo::new();
    let id = open(&state, &repo);
    let out = add(
        &state,
        &repo,
        &id,
        serde_json::json!({"path": "old.txt", "side": "base", "line": 1, "body": "x"}),
    );
    let body: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(body["error"]["code"], "E_INVALID");
    assert!(body["error"]["message"].as_str().unwrap().contains("new.txt"));
}

#[test]
fn a_reply_needs_no_line_that_still_exists() {
    let state = tempfile::tempdir().unwrap();
    let repo = Repo::new();
    let id = open(&state, &repo);
    let body = json(&add(
        &state,
        &repo,
        &id,
        serde_json::json!({"path": "gone.txt", "side": "head", "line": 40, "body": "still relevant", "reply_to": "PRRT_1"}),
    ));
    let reply = &body["session"]["comments"][0];
    assert_eq!(reply["reply_to"], "PRRT_1");
    assert_eq!(reply["commentable"]["github"], true);
    assert_eq!(reply["stale"], false);

    let refreshed = json(
        &reviewkit(&state, &repo)
            .args(["session", "refresh", "--session", &id])
            .output()
            .unwrap(),
    );
    assert_eq!(refreshed["session"]["comments"][0]["stale"], false);
    assert_eq!(refreshed["session"]["comments"][0]["line"], 40);

    let reply_id = reply["id"].as_str().unwrap().to_string();
    let moved = reviewkit(&state, &repo)
        .args(["comment", "update", "--session", &id, "--id", &reply_id])
        .write_stdin(r#"{"line": 2}"#)
        .output()
        .unwrap();
    assert_eq!(moved.status.code(), Some(2));
    let edited = json(
        &reviewkit(&state, &repo)
            .args(["comment", "update", "--session", &id, "--id", &reply_id])
            .write_stdin(r#"{"body": "edited"}"#)
            .output()
            .unwrap(),
    );
    assert_eq!(edited["session"]["comments"][0]["body"], "edited");

    let line_comment = add(
        &state,
        &repo,
        &id,
        serde_json::json!({"path": "gone.txt", "side": "head", "line": 1, "body": "not a reply"}),
    );
    assert!(!line_comment.status.success());
}

#[test]
fn a_comment_needs_a_body_or_a_suggestion() {
    let state = tempfile::tempdir().unwrap();
    let repo = Repo::new();
    let id = open(&state, &repo);
    let blank = add(
        &state,
        &repo,
        &id,
        serde_json::json!({"path": "keep.txt", "side": "head", "line": 3, "body": "  \n"}),
    );
    assert_eq!(blank.status.code(), Some(2));
    let only_suggestion = json(&add(
        &state,
        &repo,
        &id,
        serde_json::json!({"path": "keep.txt", "side": "head", "line": 3, "body": "", "suggestion": "c"}),
    ));
    let cid = only_suggestion["data"]["id"].as_str().unwrap().to_string();
    let emptied = reviewkit(&state, &repo)
        .args(["comment", "update", "--session", &id, "--id", &cid])
        .write_stdin(r#"{"suggestion": null}"#)
        .output()
        .unwrap();
    assert_eq!(emptied.status.code(), Some(2));
}

#[test]
fn an_empty_suggestion_proposes_deleting_the_lines() {
    let state = tempfile::tempdir().unwrap();
    let repo = Repo::new();
    let id = open(&state, &repo);
    let body = json(&add(
        &state,
        &repo,
        &id,
        serde_json::json!({"path": "keep.txt", "side": "head", "line": 3, "body": "", "suggestion": ""}),
    ));
    assert_eq!(body["session"]["comments"][0]["suggestion"], "");
}

#[test]
fn a_range_of_one_line_is_stored_as_a_single_line_comment() {
    let state = tempfile::tempdir().unwrap();
    let repo = Repo::new();
    let id = open(&state, &repo);
    let body = json(&add(
        &state,
        &repo,
        &id,
        serde_json::json!({"path": "keep.txt", "side": "head", "line": 3, "start_line": 3, "body": "x"}),
    ));
    assert!(body["session"]["comments"][0]["start_line"].is_null());
    let cid = body["data"]["id"].as_str().unwrap();
    let moved = json(
        &reviewkit(&state, &repo)
            .args(["comment", "update", "--session", &id, "--id", cid])
            .write_stdin(r#"{"line": 2, "start_line": 2}"#)
            .output()
            .unwrap(),
    );
    assert!(moved["session"]["comments"][0]["start_line"].is_null());
}
