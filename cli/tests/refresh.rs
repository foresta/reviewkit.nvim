mod common;

use common::{Repo, json, reviewkit};

fn open_worktree(state: &tempfile::TempDir, repo: &Repo) -> String {
    let opened = json(
        &reviewkit(state, repo)
            .args(["session", "open", "--local"])
            .output()
            .unwrap(),
    );
    opened["session"]["id"].as_str().unwrap().to_string()
}

fn add(
    state: &tempfile::TempDir,
    repo: &Repo,
    id: &str,
    path: &str,
    side: &str,
    line: u32,
    start: Option<u32>,
) -> serde_json::Value {
    let mut input = serde_json::json!({"path": path, "side": side, "line": line, "body": "note"});
    if let Some(start) = start {
        input["start_line"] = serde_json::json!(start);
    }
    json(
        &reviewkit(state, repo)
            .args(["comment", "add", "--session", id])
            .write_stdin(input.to_string())
            .output()
            .unwrap(),
    )
}

fn refresh(state: &tempfile::TempDir, repo: &Repo, id: &str) -> serde_json::Value {
    json(
        &reviewkit(state, repo)
            .args(["session", "refresh", "--session", id])
            .output()
            .unwrap(),
    )
}

#[test]
fn a_comment_follows_lines_inserted_above_it() {
    let state = tempfile::tempdir().unwrap();
    let repo = Repo::new();
    repo.write("keep.txt", "a\nb\nC\nd\ne\nf\ng\nh\ni\nj\nk\nz\n");
    let id = open_worktree(&state, &repo);
    add(&state, &repo, &id, "keep.txt", "head", 3, None);
    repo.write("keep.txt", "new1\nnew2\na\nb\nC\nd\ne\nf\ng\nh\ni\nj\nk\nz\n");
    let body = refresh(&state, &repo, &id);
    let c = &body["session"]["comments"][0];
    assert_eq!(c["line"], 5);
    assert_eq!(c["stale"], false);
    assert_eq!(c["anchor"]["context"]["lines"], serde_json::json!(["C"]));
    assert_eq!(c["anchor"]["context"]["before"], serde_json::json!(["a", "b"]));
    assert_eq!(c["anchor"]["blob"], body["session"]["targets"][0]["head_blob"]);
}

#[test]
fn editing_the_commented_line_marks_it_stale_and_keeps_the_old_line() {
    let state = tempfile::tempdir().unwrap();
    let repo = Repo::new();
    repo.write("keep.txt", "a\nb\nC\nd\ne\nf\ng\nh\ni\nj\nk\nz\n");
    let id = open_worktree(&state, &repo);
    add(&state, &repo, &id, "keep.txt", "head", 3, None);
    repo.write("keep.txt", "a\nb\nCHANGED\nd\ne\nf\ng\nh\ni\nj\nk\nz\n");
    let body = refresh(&state, &repo, &id);
    let c = &body["session"]["comments"][0];
    assert_eq!(c["stale"], true);
    assert_eq!(c["line"], 3);
    assert_eq!(
        c["anchor"]["context"]["lines"],
        serde_json::json!(["C"]),
        "the anchor keeps what the comment was about"
    );
}

#[test]
fn unchanged_files_and_edits_below_leave_the_comment_alone() {
    let state = tempfile::tempdir().unwrap();
    let repo = Repo::new();
    repo.write("keep.txt", "a\nb\nC\nd\ne\nf\ng\nh\ni\nj\nk\nz\n");
    let id = open_worktree(&state, &repo);
    let added = add(&state, &repo, &id, "keep.txt", "head", 3, None);
    let before = added["session"]["etags"]["comments"].clone();
    let same = refresh(&state, &repo, &id);
    assert_eq!(
        same["session"]["etags"]["comments"], before,
        "nothing changed, nothing rewritten"
    );
    repo.write("keep.txt", "a\nb\nC\nd\ne\nf\ng\nh\ni\nj\nk\nz\nzz\n");
    let below = refresh(&state, &repo, &id);
    let c = &below["session"]["comments"][0];
    assert_eq!((c["line"].as_u64(), c["stale"].as_bool()), (Some(3), Some(false)));
}

#[test]
fn a_range_comment_shifts_as_a_whole_and_goes_stale_when_split() {
    let state = tempfile::tempdir().unwrap();
    let repo = Repo::new();
    repo.write("keep.txt", "a\nb\nC\nd\ne\nf\ng\nh\ni\nj\nk\nz\n");
    let id = open_worktree(&state, &repo);
    add(&state, &repo, &id, "keep.txt", "head", 4, Some(2));
    repo.write("keep.txt", "top\na\nb\nC\nd\ne\nf\ng\nh\ni\nj\nk\nz\n");
    let shifted = refresh(&state, &repo, &id);
    let c = &shifted["session"]["comments"][0];
    assert_eq!((c["start_line"].as_u64(), c["line"].as_u64()), (Some(3), Some(5)));
    repo.write("keep.txt", "top\na\nb\ninside\nC\nd\ne\nf\ng\nh\ni\nj\nk\nz\n");
    let split = refresh(&state, &repo, &id);
    assert_eq!(split["session"]["comments"][0]["stale"], true);
}

#[test]
fn committing_moves_a_head_base_and_base_side_comments_follow_it() {
    let state = tempfile::tempdir().unwrap();
    let repo = Repo::new();
    let opened = json(
        &reviewkit(&state, &repo)
            .args(["session", "open", "--local", "--base", "HEAD~1"])
            .output()
            .unwrap(),
    );
    let id = opened["session"]["id"].as_str().unwrap().to_string();
    let old_base = opened["session"]["base"]["sha"].clone();
    let added = add(&state, &repo, &id, "keep.txt", "base", 5, None);
    let old_blob = added["session"]["comments"][0]["anchor"]["blob"].clone();
    repo.write("keep.txt", "zero\na\nb\nC\nd\ne\nf\ng\nh\ni\nj\nk\n");
    repo.git(&["commit", "-q", "-am", "move"]);
    let body = refresh(&state, &repo, &id);
    assert_ne!(body["session"]["base"]["sha"], old_base, "HEAD~1 moved with the commit");
    let c = &body["session"]["comments"][0];
    assert_eq!(c["line"], 5, "line 3 changed in the new base but line 5 did not");
    assert_eq!(c["stale"], false);
    assert_ne!(c["anchor"]["blob"], old_blob);
    assert_eq!(c["anchor"]["base_sha"], body["session"]["base"]["sha"]);
    assert_eq!(c["anchor"]["context"]["lines"], serde_json::json!(["e"]));
}

#[test]
fn a_deleted_file_makes_its_comments_stale() {
    let state = tempfile::tempdir().unwrap();
    let repo = Repo::new();
    repo.write("keep.txt", "a\nb\nC\nd\n");
    let id = open_worktree(&state, &repo);
    add(&state, &repo, &id, "keep.txt", "head", 3, None);
    std::fs::remove_file(repo.path().join("keep.txt")).unwrap();
    let body = refresh(&state, &repo, &id);
    assert_eq!(body["session"]["comments"][0]["stale"], true);
    assert_eq!(body["session"]["comments"][0]["commentable"]["github"], false);
}

#[test]
fn moving_the_line_back_with_update_clears_stale() {
    let state = tempfile::tempdir().unwrap();
    let repo = Repo::new();
    repo.write("keep.txt", "a\nb\nC\nd\n");
    let id = open_worktree(&state, &repo);
    let added = add(&state, &repo, &id, "keep.txt", "head", 3, None);
    let cid = added["data"]["id"].as_str().unwrap().to_string();
    repo.write("keep.txt", "a\nb\nCC\nd\n");
    assert_eq!(refresh(&state, &repo, &id)["session"]["comments"][0]["stale"], true);
    let fixed = json(
        &reviewkit(&state, &repo)
            .args(["comment", "update", "--session", &id, "--id", &cid])
            .write_stdin(r#"{"line": 3}"#)
            .output()
            .unwrap(),
    );
    assert_eq!(fixed["session"]["comments"][0]["stale"], false);
    assert_eq!(
        fixed["session"]["comments"][0]["anchor"]["context"]["lines"],
        serde_json::json!(["CC"])
    );
}

#[test]
fn a_pruned_old_blob_degrades_to_a_text_check_instead_of_failing() {
    let state = tempfile::tempdir().unwrap();
    let repo = Repo::new();
    repo.write("keep.txt", "a\nb\nC\nd\n");
    let id = open_worktree(&state, &repo);
    let added = add(&state, &repo, &id, "keep.txt", "head", 3, None);
    let blob = added["session"]["comments"][0]["anchor"]["blob"]
        .as_str()
        .unwrap()
        .to_string();
    let object = repo.path().join(".git/objects").join(&blob[..2]).join(&blob[2..]);
    std::fs::remove_file(&object).expect("loose object written by hash-object -w");
    repo.write("keep.txt", "a\nb\nC\nd\nmore\n");
    let body = refresh(&state, &repo, &id);
    let c = &body["session"]["comments"][0];
    assert_eq!((c["line"].as_u64(), c["stale"].as_bool()), (Some(3), Some(false)));
    repo.write("keep.txt", "top\na\nb\nC\nd\nmore\n");
    std::fs::remove_file(
        repo.path()
            .join(".git/objects")
            .join(&c["anchor"]["blob"].as_str().unwrap()[..2])
            .join(&c["anchor"]["blob"].as_str().unwrap()[2..]),
    )
    .ok();
    let shifted = refresh(&state, &repo, &id);
    assert_eq!(
        shifted["session"]["comments"][0]["stale"], true,
        "without the diff a moved line cannot be found"
    );
}

#[test]
fn committed_head_sessions_follow_a_new_head_commit() {
    let state = tempfile::tempdir().unwrap();
    let repo = Repo::new();
    repo.git(&["branch", "review"]);
    let opened = json(
        &reviewkit(&state, &repo)
            .args(["session", "open", "--local", "--base", "HEAD~1", "--head", "review"])
            .output()
            .unwrap(),
    );
    let id = opened["session"]["id"].as_str().unwrap().to_string();
    add(&state, &repo, &id, "keep.txt", "head", 3, None);
    repo.write("keep.txt", "zero\na\nb\nC\nd\ne\nf\ng\nh\ni\nj\nk\n");
    repo.git(&["commit", "-q", "-am", "more"]);
    repo.git(&["branch", "-f", "review", "HEAD"]);
    let body = refresh(&state, &repo, &id);
    assert_eq!(body["session"]["head"]["sha"].as_str().unwrap().len(), 40);
    let c = &body["session"]["comments"][0];
    assert_eq!((c["line"].as_u64(), c["stale"].as_bool()), (Some(4), Some(false)));
    assert_eq!(c["anchor"]["head_sha"], body["session"]["head"]["sha"]);
}

#[test]
fn reverting_a_file_keeps_the_comment_though_it_is_no_longer_a_target() {
    let state = tempfile::tempdir().unwrap();
    let repo = Repo::new();
    repo.write("keep.txt", "a\nb\nC\nd\ne\nf\ng\nh\ni\nj\nk\nz\n");
    let id = open_worktree(&state, &repo);
    add(&state, &repo, &id, "keep.txt", "head", 3, None);
    repo.git(&["checkout", "--", "keep.txt"]);
    let body = refresh(&state, &repo, &id);
    assert!(
        body["session"]["targets"]
            .as_array()
            .unwrap()
            .iter()
            .all(|t| t["path"] != "keep.txt")
    );
    let c = &body["session"]["comments"][0];
    assert_eq!(c["stale"], false, "line 3 still reads C in the committed file");
    assert_eq!(
        c["commentable"],
        serde_json::json!({"github": false, "reason": "outside_diff"})
    );
}

#[test]
fn an_empty_file_comment_survives_content_being_added_after_it() {
    let state = tempfile::tempdir().unwrap();
    let repo = Repo::new();
    repo.write("empty.txt", "");
    repo.git(&["add", "empty.txt"]);
    let id = open_worktree(&state, &repo);
    add(&state, &repo, &id, "empty.txt", "head", 1, None);
    repo.write("empty.txt", "now\n");
    let body = refresh(&state, &repo, &id);
    let c = &body["session"]["comments"][0];
    assert_eq!(c["stale"], true, "the virtual first line was replaced by real content");
    assert_eq!(c["line"], 1);
}

#[test]
fn a_stale_comment_recovers_when_its_text_comes_back() {
    let state = tempfile::tempdir().unwrap();
    let repo = Repo::new();
    repo.write("keep.txt", "a\nb\nC\nd\n");
    let id = open_worktree(&state, &repo);
    add(&state, &repo, &id, "keep.txt", "head", 3, None);
    repo.write("keep.txt", "a\nb\nX\nd\n");
    assert_eq!(refresh(&state, &repo, &id)["session"]["comments"][0]["stale"], true);
    repo.write("keep.txt", "a\nb\nC\nd\n");
    assert_eq!(refresh(&state, &repo, &id)["session"]["comments"][0]["stale"], false);
}

#[test]
fn comments_follow_a_worktree_rename() {
    let state = tempfile::tempdir().unwrap();
    let repo = Repo::new();
    let opened = json(
        &reviewkit(&state, &repo)
            .args(["session", "open", "--local", "--base", "HEAD~1"])
            .output()
            .unwrap(),
    );
    let id = opened["session"]["id"].as_str().unwrap().to_string();
    add(&state, &repo, &id, "keep.txt", "head", 3, None);
    add(&state, &repo, &id, "keep.txt", "base", 3, None);
    repo.git(&["mv", "keep.txt", "kept.txt"]);
    let body = refresh(&state, &repo, &id);
    for c in body["session"]["comments"].as_array().unwrap() {
        assert_eq!(c["path"], "kept.txt");
        assert_eq!(c["stale"], false, "{}", c["side"]);
    }
    assert_eq!(body["session"]["comments"][0]["commentable"]["github"], true);
}
