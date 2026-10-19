mod common;

use common::json;
use common::pr::{FakeGh, Remote, reviewkit, thread};

fn open(state: &tempfile::TempDir, remote: &Remote, gh: &FakeGh) -> String {
    let body = json(
        &reviewkit(state, remote, gh)
            .args(["session", "open", "--pr", "https://github.com/o/r/pull/7"])
            .output()
            .unwrap(),
    );
    body["session"]["id"].as_str().unwrap().to_string()
}

fn add(state: &tempfile::TempDir, remote: &Remote, gh: &FakeGh, id: &str, input: serde_json::Value) -> String {
    let body = json(
        &reviewkit(state, remote, gh)
            .args(["comment", "add", "--session", id])
            .write_stdin(input.to_string())
            .output()
            .unwrap(),
    );
    body["data"]["id"].as_str().unwrap().to_string()
}

/// Prepends a rule answering the mutation; the query rule from FakeGh::new
/// stays for `pr view` and the details query.
fn answer_mutation(gh: &FakeGh, body: serde_json::Value, exit: i64) {
    let mut rules: Vec<serde_json::Value> =
        serde_json::from_str(&std::fs::read_to_string(gh.dir.path().join("responses.json")).unwrap()).unwrap();
    rules.insert(
        0,
        serde_json::json!({ "match": ["api graphql", "mutation("], "body": body, "exit": exit }),
    );
    std::fs::write(
        gh.dir.path().join("responses.json"),
        serde_json::Value::Array(rules).to_string(),
    )
    .unwrap();
}

fn mutation_calls(gh: &FakeGh) -> Vec<serde_json::Value> {
    gh.calls()
        .into_iter()
        .filter(|c| c["stdin"].as_str().is_some_and(|s| s.contains("mutation(")))
        .map(|c| serde_json::from_str::<serde_json::Value>(c["stdin"].as_str().unwrap()).unwrap())
        .collect()
}

fn decide(state: &tempfile::TempDir, remote: &Remote, gh: &FakeGh, id: &str, event: &str, body: &str) {
    json(
        &reviewkit(state, remote, gh)
            .args(["decision", "set", "--session", id])
            .write_stdin(serde_json::json!({ "event": event, "body": body }).to_string())
            .output()
            .unwrap(),
    );
}

#[test]
fn submit_sends_one_review_and_clears_what_github_accepted() {
    let state = tempfile::tempdir().unwrap();
    let remote = Remote::new();
    let gh = FakeGh::new(
        &remote,
        "OPEN",
        serde_json::json!([thread("keep.txt", Some(3), "RIGHT", false)]),
    );
    let id = open(&state, &remote, &gh);
    let inside = add(
        &state,
        &remote,
        &gh,
        &id,
        serde_json::json!({"path": "keep.txt", "side": "head", "line": 3, "body": "why C?", "suggestion": "c"}),
    );
    let outside = add(
        &state,
        &remote,
        &gh,
        &id,
        serde_json::json!({"path": "keep.txt", "side": "head", "line": 7, "body": "far away"}),
    );
    let reply = add(
        &state,
        &remote,
        &gh,
        &id,
        serde_json::json!({"path": "keep.txt", "side": "head", "line": 3, "body": "agreed", "reply_to": "PRRT_keep.txt"}),
    );
    decide(&state, &remote, &gh, &id, "request_changes", "please fix");
    answer_mutation(
        &gh,
        serde_json::json!({ "data": {
        "review": { "pullRequestReview": { "id": "PRR_1", "url": "https://github.com/o/r/pull/7#pullrequestreview-1" } },
        "reply0": { "comment": { "id": "PRRC_9", "url": "https://github.com/o/r/pull/7#discussion_r9" } }
    } }),
        0,
    );

    let body = json(
        &reviewkit(&state, &remote, &gh)
            .args(["submit", "--session", &id, "--sink", "github"])
            .output()
            .unwrap(),
    );
    let result = &body["data"];
    assert_eq!(result["decision"], "sent");
    assert_eq!(result["url"], "https://github.com/o/r/pull/7#pullrequestreview-1");
    let sent: Vec<&str> = result["sent"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["comment_id"].as_str().unwrap())
        .collect();
    assert_eq!(sent, vec![inside.as_str(), reply.as_str()]);
    assert_eq!(result["sent"][1]["remote_id"], "PRRC_9");
    assert_eq!(result["skipped"][0]["comment_id"], outside);
    assert_eq!(result["skipped"][0]["reason"], "outside_diff");
    assert_eq!(
        result["summary"],
        "2 sent, 1 skipped (outside diff), 0 failed, decision sent"
    );

    let session = &body["session"];
    let left: Vec<&str> = session["comments"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["id"].as_str().unwrap())
        .collect();
    assert_eq!(left, vec![outside.as_str()], "only the skipped comment stays");
    assert!(session["decision"].is_null());
    assert_eq!(session["last_submit"]["sink"], "github");

    let sent_mutations = mutation_calls(&gh);
    assert_eq!(sent_mutations.len(), 1);
    let vars = &sent_mutations[0]["variables"];
    assert_eq!(vars["event"], "REQUEST_CHANGES");
    assert_eq!(vars["body"], "please fix");
    assert_eq!(vars["prId"], "PR_node");
    assert_eq!(vars["commit"], remote.head_sha);
    assert_eq!(vars["threads"].as_array().unwrap().len(), 1);
    assert_eq!(vars["threads"][0]["side"], "RIGHT");
    assert!(
        vars["threads"][0]["body"]
            .as_str()
            .unwrap()
            .contains("```suggestion\nc\n```")
    );
    assert_eq!(vars["reply0"]["pullRequestReviewThreadId"], "PRRT_keep.txt");
    assert!(
        !state
            .path()
            .join("sessions/pr/github.com/o/r/7/pending_submit.json")
            .exists()
    );
}

#[test]
fn dry_run_returns_the_plan_and_sends_nothing() {
    let state = tempfile::tempdir().unwrap();
    let remote = Remote::new();
    let gh = FakeGh::new(&remote, "OPEN", serde_json::json!([]));
    let id = open(&state, &remote, &gh);
    add(
        &state,
        &remote,
        &gh,
        &id,
        serde_json::json!({"path": "keep.txt", "side": "head", "line": 3, "body": "hm"}),
    );
    let body = json(
        &reviewkit(&state, &remote, &gh)
            .args(["submit", "--session", &id, "--sink", "github", "--dry-run"])
            .output()
            .unwrap(),
    );
    assert_eq!(body["data"]["preview"]["event"], "COMMENT");
    assert_eq!(body["data"]["preview"]["variables"]["body"], "");
    assert!(
        body["data"]["preview"]["mutation"]
            .as_str()
            .unwrap()
            .starts_with("mutation(")
    );
    assert!(mutation_calls(&gh).is_empty());
    assert!(body.get("session").is_none());
}

#[test]
fn a_whole_review_failure_keeps_the_queue_and_names_a_pending_review() {
    let state = tempfile::tempdir().unwrap();
    let remote = Remote::new();
    let gh = FakeGh::new(&remote, "OPEN", serde_json::json!([]));
    let id = open(&state, &remote, &gh);
    add(
        &state,
        &remote,
        &gh,
        &id,
        serde_json::json!({"path": "keep.txt", "side": "head", "line": 3, "body": "hm"}),
    );
    decide(&state, &remote, &gh, &id, "approve", "ok");
    answer_mutation(
        &gh,
        serde_json::json!({ "data": { "review": null }, "errors": [ { "message": "A pending review already exists for this pull request", "path": ["review"] } ] }),
        1,
    );
    let body = json(
        &reviewkit(&state, &remote, &gh)
            .args(["submit", "--session", &id, "--sink", "github"])
            .output()
            .unwrap(),
    );
    assert_eq!(body["ok"], true, "the request was sent; the outcome is in the result");
    assert_eq!(
        body["data"]["failed"].as_array().unwrap().len(),
        1,
        "one entry per inline comment"
    );
    assert!(body["data"]["failed"][0]["comment_id"].is_string());
    assert_eq!(body["data"]["failed"][0]["error"]["code"], "E_GH_PENDING_REVIEW");
    assert_eq!(body["data"]["decision"], "failed");
    assert_eq!(body["session"]["comments"].as_array().unwrap().len(), 1);
    assert_eq!(
        body["session"]["comments"][0]["last_error"]["code"],
        "E_GH_PENDING_REVIEW"
    );
    assert!(body["session"]["decision"].is_object());
}

#[test]
fn a_failed_reply_is_reported_alone_while_the_review_goes_through() {
    let state = tempfile::tempdir().unwrap();
    let remote = Remote::new();
    let gh = FakeGh::new(
        &remote,
        "OPEN",
        serde_json::json!([thread("keep.txt", Some(3), "RIGHT", false)]),
    );
    let id = open(&state, &remote, &gh);
    let inline = add(
        &state,
        &remote,
        &gh,
        &id,
        serde_json::json!({"path": "keep.txt", "side": "head", "line": 3, "body": "hm"}),
    );
    let reply = add(
        &state,
        &remote,
        &gh,
        &id,
        serde_json::json!({"path": "keep.txt", "side": "head", "line": 3, "body": "re", "reply_to": "PRRT_gone"}),
    );
    answer_mutation(
        &gh,
        serde_json::json!({
            "data": { "review": { "pullRequestReview": { "id": "PRR_1", "url": "u" } }, "reply0": null },
            "errors": [ { "message": "Could not resolve to a node with the global id of 'PRRT_gone'", "path": ["reply0"] } ]
        }),
        1,
    );
    let body = json(
        &reviewkit(&state, &remote, &gh)
            .args(["submit", "--session", &id, "--sink", "github"])
            .output()
            .unwrap(),
    );
    assert_eq!(body["data"]["sent"][0]["comment_id"], inline);
    assert_eq!(body["data"]["failed"][0]["comment_id"], reply);
    assert_eq!(body["data"]["failed"][0]["error"]["code"], "E_GH");
    assert_eq!(body["session"]["comments"].as_array().unwrap().len(), 1);
    assert_eq!(body["session"]["comments"][0]["id"], reply);
    let message = body["session"]["comments"][0]["last_error"]["message"]
        .as_str()
        .unwrap();
    assert!(message.contains("PRRT_gone"));
}

#[test]
fn an_interrupted_submit_is_reconciled_against_github_before_resending() {
    let state = tempfile::tempdir().unwrap();
    let remote = Remote::new();
    let gh = FakeGh::new(&remote, "OPEN", serde_json::json!([]));
    let id = open(&state, &remote, &gh);
    let posted = add(
        &state,
        &remote,
        &gh,
        &id,
        serde_json::json!({"path": "keep.txt", "side": "head", "line": 3, "body": "already there"}),
    );
    let fresh = add(
        &state,
        &remote,
        &gh,
        &id,
        serde_json::json!({"path": "keep.txt", "side": "head", "line": 11, "body": "new one"}),
    );
    // The previous run died after GitHub created the thread but before the answer was applied.
    let pending = serde_json::json!({ "at": "2026-01-01T00:00:00Z", "comments": [ { "id": posted, "path": "keep.txt", "side": "head", "line": 3, "body": "already there" } ] });
    std::fs::write(
        state.path().join("sessions/pr/github.com/o/r/7/pending_submit.json"),
        pending.to_string(),
    )
    .unwrap();
    // GitHub now shows that thread.
    let mut on_github = thread("keep.txt", Some(3), "RIGHT", false);
    on_github["comments"]["nodes"][0]["body"] = serde_json::json!("already there");
    let mut rules: Vec<serde_json::Value> =
        serde_json::from_str(&std::fs::read_to_string(gh.dir.path().join("responses.json")).unwrap()).unwrap();
    for rule in rules.iter_mut() {
        let nodes = &mut rule["body"]["data"]["repository"]["pullRequest"]["reviewThreads"]["nodes"];
        if nodes.is_array() {
            *nodes = serde_json::json!([on_github]);
        }
    }
    std::fs::write(
        gh.dir.path().join("responses.json"),
        serde_json::Value::Array(rules).to_string(),
    )
    .unwrap();
    answer_mutation(
        &gh,
        serde_json::json!({ "data": { "review": { "pullRequestReview": { "id": "PRR_2", "url": "u2" } } } }),
        0,
    );

    let body = json(
        &reviewkit(&state, &remote, &gh)
            .args(["submit", "--session", &id, "--sink", "github"])
            .output()
            .unwrap(),
    );
    let sent: Vec<&str> = body["data"]["sent"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["comment_id"].as_str().unwrap())
        .collect();
    assert_eq!(
        sent,
        vec![posted.as_str(), fresh.as_str()],
        "the recovered one is reported too"
    );
    assert!(
        body["session"]["comments"].as_array().unwrap().is_empty(),
        "the reconciled one left the queue too"
    );
    let vars = &mutation_calls(&gh)[0]["variables"];
    assert_eq!(vars["threads"].as_array().unwrap().len(), 1);
    assert_eq!(vars["threads"][0]["line"], 11);
}

#[test]
fn local_sessions_cannot_use_the_github_sink_and_sink_list_names_it() {
    let state = tempfile::tempdir().unwrap();
    let remote = Remote::new();
    let gh = FakeGh::new(&remote, "OPEN", serde_json::json!([]));
    let local = json(
        &reviewkit(&state, &remote, &gh)
            .args(["session", "open", "--local"])
            .output()
            .unwrap(),
    );
    let id = local["session"]["id"].as_str().unwrap().to_string();
    add(
        &state,
        &remote,
        &gh,
        &id,
        serde_json::json!({"path": "keep.txt", "side": "head", "line": 1, "body": "x"}),
    );
    let out = reviewkit(&state, &remote, &gh)
        .args(["submit", "--session", &id, "--sink", "github"])
        .output()
        .unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&out.stdout).unwrap()["error"]["code"],
        "E_INVALID"
    );
    let list = json(&reviewkit(&state, &remote, &gh).args(["sink", "list"]).output().unwrap());
    assert_eq!(list["data"][1]["name"], "github");
    assert_eq!(list["data"][1]["available"], true);
}

/// Rewrites the details query of FakeGh: GitHub's threads and timeline as
/// the reconcile will see them, plus the viewer login.
fn github_shows(gh: &FakeGh, viewer: &str, threads: serde_json::Value, reviews: serde_json::Value) {
    let mut rules: Vec<serde_json::Value> =
        serde_json::from_str(&std::fs::read_to_string(gh.dir.path().join("responses.json")).unwrap()).unwrap();
    for rule in rules.iter_mut() {
        let data = &mut rule["body"]["data"];
        if data["repository"]["pullRequest"]["reviewThreads"].is_object() {
            data["viewer"] = serde_json::json!({ "login": viewer });
            data["repository"]["pullRequest"]["reviewThreads"]["nodes"] = threads.clone();
            data["repository"]["pullRequest"]["reviews"]["nodes"] = reviews.clone();
        }
    }
    std::fs::write(
        gh.dir.path().join("responses.json"),
        serde_json::Value::Array(rules).to_string(),
    )
    .unwrap();
}

fn pending(state: &tempfile::TempDir, value: serde_json::Value) {
    std::fs::write(
        state.path().join("sessions/pr/github.com/o/r/7/pending_submit.json"),
        value.to_string(),
    )
    .unwrap();
}

fn posted_thread(path: &str, line: u32, author: &str, body: &str) -> serde_json::Value {
    let mut t = thread(path, Some(line), "RIGHT", false);
    t["comments"]["nodes"][0]["body"] = serde_json::json!(body);
    t["comments"]["nodes"][0]["author"] = serde_json::json!({ "login": author });
    t
}

#[test]
fn when_everything_was_already_posted_nothing_is_sent_and_the_record_is_cleared() {
    let state = tempfile::tempdir().unwrap();
    let remote = Remote::new();
    let gh = FakeGh::new(&remote, "OPEN", serde_json::json!([]));
    let id = open(&state, &remote, &gh);
    let posted = add(
        &state,
        &remote,
        &gh,
        &id,
        serde_json::json!({"path": "keep.txt", "side": "head", "line": 3, "body": "done"}),
    );
    pending(
        &state,
        serde_json::json!({ "at": "2026-01-01T00:00:00Z", "comments": [ { "id": posted, "path": "keep.txt", "side": "head", "line": 3, "body": "done" } ] }),
    );
    github_shows(
        &gh,
        "me",
        serde_json::json!([posted_thread("keep.txt", 3, "me", "done\r\n")]),
        serde_json::json!([]),
    );
    let body = json(
        &reviewkit(&state, &remote, &gh)
            .args(["submit", "--session", &id, "--sink", "github"])
            .output()
            .unwrap(),
    );
    assert_eq!(body["data"]["sent"][0]["comment_id"], posted);
    assert!(body["session"]["comments"].as_array().unwrap().is_empty());
    assert!(mutation_calls(&gh).is_empty(), "nothing is posted twice");
    assert!(
        !state
            .path()
            .join("sessions/pr/github.com/o/r/7/pending_submit.json")
            .exists()
    );
    // A second submit has nothing to do and posts nothing.
    let again = reviewkit(&state, &remote, &gh)
        .args(["submit", "--session", &id, "--sink", "github"])
        .output()
        .unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&again.stdout).unwrap()["error"]["code"],
        "E_INVALID"
    );
    assert!(mutation_calls(&gh).is_empty());
}

#[test]
fn an_interrupted_reply_and_decision_are_recognised_and_not_resent() {
    let state = tempfile::tempdir().unwrap();
    let remote = Remote::new();
    let gh = FakeGh::new(
        &remote,
        "OPEN",
        serde_json::json!([thread("keep.txt", Some(3), "RIGHT", false)]),
    );
    let id = open(&state, &remote, &gh);
    let reply = add(
        &state,
        &remote,
        &gh,
        &id,
        serde_json::json!({"path": "keep.txt", "side": "head", "line": 3, "body": "agreed", "reply_to": "PRRT_keep.txt"}),
    );
    decide(&state, &remote, &gh, &id, "approve", "lgtm");
    pending(
        &state,
        serde_json::json!({
            "at": "2026-01-01T00:00:00.000000Z",
            "comments": [],
            "replies": [ { "id": reply, "thread_id": "PRRT_keep.txt", "body": "agreed" } ],
            "event": "APPROVE",
            "decision_body": "lgtm"
        }),
    );
    let mut existing = thread("keep.txt", Some(3), "RIGHT", false);
    existing["comments"]["nodes"] = serde_json::json!([
        { "id": "PRRC_1", "body": "hm", "createdAt": "t", "url": "u", "author": {"login": "bob"} },
        { "id": "PRRC_2", "body": "agreed", "createdAt": "2026-01-01T00:00:01Z", "url": "u", "author": {"login": "me"} }
    ]);
    github_shows(
        &gh,
        "me",
        serde_json::json!([existing]),
        serde_json::json!([
            { "state": "APPROVED", "body": "lgtm", "submittedAt": "2026-01-01T00:00:01Z", "url": "r", "author": {"login": "me"} }
        ]),
    );
    let body = json(
        &reviewkit(&state, &remote, &gh)
            .args(["submit", "--session", &id, "--sink", "github"])
            .output()
            .unwrap(),
    );
    assert_eq!(body["data"]["sent"][0]["comment_id"], reply);
    assert_eq!(body["data"]["decision"], "sent");
    assert!(body["session"]["decision"].is_null());
    assert!(mutation_calls(&gh).is_empty());
}

#[test]
fn someone_elses_identical_thread_does_not_count_as_ours() {
    let state = tempfile::tempdir().unwrap();
    let remote = Remote::new();
    let gh = FakeGh::new(&remote, "OPEN", serde_json::json!([]));
    let id = open(&state, &remote, &gh);
    let mine = add(
        &state,
        &remote,
        &gh,
        &id,
        serde_json::json!({"path": "keep.txt", "side": "head", "line": 3, "body": "same"}),
    );
    pending(
        &state,
        serde_json::json!({ "at": "2026-01-01T00:00:00Z", "comments": [ { "id": mine, "path": "keep.txt", "side": "head", "line": 3, "body": "same" } ] }),
    );
    github_shows(
        &gh,
        "me",
        serde_json::json!([posted_thread("keep.txt", 3, "bob", "same")]),
        serde_json::json!([]),
    );
    answer_mutation(
        &gh,
        serde_json::json!({ "data": { "review": { "pullRequestReview": { "id": "PRR_3", "url": "u3" } } } }),
        0,
    );
    let body = json(
        &reviewkit(&state, &remote, &gh)
            .args(["submit", "--session", &id, "--sink", "github"])
            .output()
            .unwrap(),
    );
    assert_eq!(body["data"]["sent"][0]["remote_id"], "PRR_3", "it was really sent");
    assert_eq!(mutation_calls(&gh).len(), 1);
}

#[test]
fn a_review_failure_marks_each_inline_comment_even_when_a_reply_succeeds() {
    let state = tempfile::tempdir().unwrap();
    let remote = Remote::new();
    let gh = FakeGh::new(
        &remote,
        "OPEN",
        serde_json::json!([thread("keep.txt", Some(3), "RIGHT", false)]),
    );
    let id = open(&state, &remote, &gh);
    let inline = add(
        &state,
        &remote,
        &gh,
        &id,
        serde_json::json!({"path": "keep.txt", "side": "head", "line": 3, "body": "hm"}),
    );
    add(
        &state,
        &remote,
        &gh,
        &id,
        serde_json::json!({"path": "keep.txt", "side": "head", "line": 3, "body": "re", "reply_to": "PRRT_keep.txt"}),
    );
    answer_mutation(
        &gh,
        serde_json::json!({
            "data": { "review": null, "reply0": { "comment": { "id": "PRRC_5", "url": "u" } } },
            "errors": [ { "message": "Something went wrong", "path": ["review"] } ]
        }),
        1,
    );
    let body = json(
        &reviewkit(&state, &remote, &gh)
            .args(["submit", "--session", &id, "--sink", "github"])
            .output()
            .unwrap(),
    );
    let comments = body["session"]["comments"].as_array().unwrap();
    assert_eq!(comments.len(), 1);
    assert_eq!(comments[0]["id"], inline);
    assert_eq!(comments[0]["last_error"]["code"], "E_GH");
}

#[test]
fn a_recovery_survives_a_follow_up_send_that_fails() {
    let state = tempfile::tempdir().unwrap();
    let remote = Remote::new();
    let gh = FakeGh::new(&remote, "OPEN", serde_json::json!([]));
    let id = open(&state, &remote, &gh);
    let posted = add(
        &state,
        &remote,
        &gh,
        &id,
        serde_json::json!({"path": "keep.txt", "side": "head", "line": 3, "body": "done"}),
    );
    let fresh = add(
        &state,
        &remote,
        &gh,
        &id,
        serde_json::json!({"path": "keep.txt", "side": "head", "line": 11, "body": "new"}),
    );
    pending(
        &state,
        serde_json::json!({ "at": "2026-01-01T00:00:00Z", "comments": [ { "id": posted, "path": "keep.txt", "side": "head", "line": 3, "body": "done" } ] }),
    );
    github_shows(
        &gh,
        "me",
        serde_json::json!([posted_thread("keep.txt", 3, "me", "done")]),
        serde_json::json!([]),
    );
    let mut rules: Vec<serde_json::Value> =
        serde_json::from_str(&std::fs::read_to_string(gh.dir.path().join("responses.json")).unwrap()).unwrap();
    rules.insert(
        0,
        serde_json::json!({ "match": ["api graphql", "mutation("], "stderr": "connection reset by peer", "exit": 1 }),
    );
    std::fs::write(
        gh.dir.path().join("responses.json"),
        serde_json::Value::Array(rules).to_string(),
    )
    .unwrap();
    let out = reviewkit(&state, &remote, &gh)
        .args(["submit", "--session", &id, "--sink", "github"])
        .output()
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(body["error"]["code"], "E_GH");
    let listed = json(
        &reviewkit(&state, &remote, &gh)
            .args(["comment", "list", "--session", &id])
            .output()
            .unwrap(),
    );
    let ids: Vec<&str> = listed["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        vec![fresh.as_str()],
        "the recovered comment left the queue for good"
    );
    let record =
        std::fs::read_to_string(state.path().join("sessions/pr/github.com/o/r/7/pending_submit.json")).unwrap();
    assert!(
        record.contains(&fresh),
        "the failed attempt is on record for the next submit"
    );
}

#[test]
fn a_decision_in_the_same_second_or_with_clock_skew_still_matches() {
    let state = tempfile::tempdir().unwrap();
    let remote = Remote::new();
    let gh = FakeGh::new(&remote, "OPEN", serde_json::json!([]));
    let id = open(&state, &remote, &gh);
    decide(&state, &remote, &gh, &id, "approve", "ok");
    pending(
        &state,
        serde_json::json!({ "at": "2026-01-01T00:00:00.400000Z", "comments": [], "event": "APPROVE", "decision_body": "ok" }),
    );
    github_shows(
        &gh,
        "me",
        serde_json::json!([]),
        serde_json::json!([
            { "state": "APPROVED", "body": "ok", "submittedAt": "2026-01-01T00:00:00Z", "url": "r", "author": {"login": "me"} }
        ]),
    );
    let body = json(
        &reviewkit(&state, &remote, &gh)
            .args(["submit", "--session", &id, "--sink", "github"])
            .output()
            .unwrap(),
    );
    assert_eq!(body["data"]["decision"], "sent");
    assert!(mutation_calls(&gh).is_empty());
}

#[test]
fn an_edit_after_the_crash_is_not_mistaken_for_what_was_posted() {
    let state = tempfile::tempdir().unwrap();
    let remote = Remote::new();
    let gh = FakeGh::new(&remote, "OPEN", serde_json::json!([]));
    let id = open(&state, &remote, &gh);
    let cid = add(
        &state,
        &remote,
        &gh,
        &id,
        serde_json::json!({"path": "keep.txt", "side": "head", "line": 3, "body": "edited later"}),
    );
    pending(
        &state,
        serde_json::json!({ "at": "2026-01-01T00:00:00Z", "comments": [ { "id": cid, "path": "keep.txt", "side": "head", "line": 3, "body": "original" } ] }),
    );
    github_shows(
        &gh,
        "me",
        serde_json::json!([posted_thread("keep.txt", 3, "me", "original")]),
        serde_json::json!([]),
    );
    answer_mutation(
        &gh,
        serde_json::json!({ "data": { "review": { "pullRequestReview": { "id": "PRR_4", "url": "u4" } } } }),
        0,
    );
    let body = json(
        &reviewkit(&state, &remote, &gh)
            .args(["submit", "--session", &id, "--sink", "github"])
            .output()
            .unwrap(),
    );
    assert_eq!(
        body["data"]["sent"][0]["remote_id"], "PRR_4",
        "the edited text is really sent"
    );
    assert_eq!(
        mutation_calls(&gh)[0]["variables"]["threads"][0]["body"],
        "edited later"
    );
}

fn thread_cmd(state: &tempfile::TempDir, remote: &Remote, gh: &FakeGh, args: &[&str]) -> std::process::Output {
    let mut full = vec!["thread"];
    full.extend_from_slice(args);
    reviewkit(state, remote, gh).args(full).output().unwrap()
}

#[test]
fn resolving_a_thread_is_queued_toggled_and_rejected_for_unknown_ids() {
    let state = tempfile::tempdir().unwrap();
    let remote = Remote::new();
    let gh = FakeGh::new(
        &remote,
        "OPEN",
        serde_json::json!([thread("keep.txt", Some(3), "RIGHT", false)]),
    );
    let id = open(&state, &remote, &gh);
    let listed = json(&thread_cmd(&state, &remote, &gh, &["list", "--session", &id]));
    assert_eq!(listed["data"][0]["id"], "PRRT_keep.txt");

    let queued = json(&thread_cmd(
        &state,
        &remote,
        &gh,
        &["resolve", "--session", &id, "--thread", "PRRT_keep.txt"],
    ));
    assert_eq!(
        queued["session"]["thread_actions"],
        serde_json::json!([{ "thread_id": "PRRT_keep.txt", "action": "resolve" }])
    );

    let undone = json(&thread_cmd(
        &state,
        &remote,
        &gh,
        &["resolve", "--session", &id, "--thread", "PRRT_keep.txt", "--undo"],
    ));
    assert_eq!(undone["session"]["thread_actions"], serde_json::json!([]));

    let missing = thread_cmd(
        &state,
        &remote,
        &gh,
        &["resolve", "--session", &id, "--thread", "PRRT_nope"],
    );
    assert_eq!(missing.status.code(), Some(3));
    let error: serde_json::Value = serde_json::from_slice(&missing.stdout).unwrap();
    assert_eq!(error["error"]["code"], "E_NO_THREAD");
}

#[test]
fn a_queued_resolve_is_sent_on_submit_and_cleared_once_github_accepts_it() {
    let state = tempfile::tempdir().unwrap();
    let remote = Remote::new();
    let gh = FakeGh::new(
        &remote,
        "OPEN",
        serde_json::json!([thread("keep.txt", Some(3), "RIGHT", false)]),
    );
    let id = open(&state, &remote, &gh);
    json(&thread_cmd(
        &state,
        &remote,
        &gh,
        &["resolve", "--session", &id, "--thread", "PRRT_keep.txt"],
    ));
    answer_mutation(
        &gh,
        serde_json::json!({ "data": { "action0": { "thread": { "id": "PRRT_keep.txt", "isResolved": true } } } }),
        0,
    );
    let body = json(
        &reviewkit(&state, &remote, &gh)
            .args(["submit", "--session", &id, "--sink", "github"])
            .output()
            .unwrap(),
    );
    assert_eq!(body["ok"], true, "{body}");
    assert_eq!(body["data"]["thread_actions"][0]["status"], "sent");
    assert_eq!(body["session"]["thread_actions"], serde_json::json!([]));
    let sent = mutation_calls(&gh);
    assert_eq!(sent.len(), 1);
    assert!(sent[0]["query"].as_str().unwrap().contains("resolveReviewThread"));
}

#[test]
fn refreshing_threads_drops_a_resolve_that_someone_already_did_on_github() {
    let state = tempfile::tempdir().unwrap();
    let remote = Remote::new();
    let gh = FakeGh::new(
        &remote,
        "OPEN",
        serde_json::json!([thread("keep.txt", Some(3), "RIGHT", false)]),
    );
    let id = open(&state, &remote, &gh);
    json(&thread_cmd(
        &state,
        &remote,
        &gh,
        &["resolve", "--session", &id, "--thread", "PRRT_keep.txt"],
    ));
    let mut resolved = thread("keep.txt", Some(3), "RIGHT", false);
    resolved["isResolved"] = serde_json::json!(true);
    github_shows(&gh, "me", serde_json::json!([resolved]), serde_json::json!([]));
    let body = json(&thread_cmd(
        &state,
        &remote,
        &gh,
        &["list", "--session", &id, "--refresh"],
    ));
    assert_eq!(body["data"][0]["resolved"], true);
    assert_eq!(body["session"]["thread_actions"], serde_json::json!([]));
}

#[test]
fn an_interrupted_comment_is_recognised_even_after_its_line_moved() {
    let state = tempfile::tempdir().unwrap();
    let remote = Remote::new();
    let gh = FakeGh::new(&remote, "OPEN", serde_json::json!([]));
    let id = open(&state, &remote, &gh);
    let cid = add(
        &state,
        &remote,
        &gh,
        &id,
        serde_json::json!({"path": "keep.txt", "side": "head", "line": 3, "body": "done"}),
    );
    pending(
        &state,
        serde_json::json!({
            "at": "2026-01-01T00:00:00Z",
            "comments": [ { "id": cid, "path": "keep.txt", "side": "head", "line": 3, "body": "done" } ]
        }),
    );
    let got = json(
        &reviewkit(&state, &remote, &gh)
            .args(["session", "get", "--session", &id])
            .output()
            .unwrap(),
    );
    assert_eq!(got["session"]["pending_submit"], true);

    // A push since then outdated the thread: GitHub no longer reports a line.
    let mut moved = posted_thread("keep.txt", 3, "me", "done");
    moved["line"] = serde_json::Value::Null;
    moved["originalLine"] = serde_json::json!(3);
    moved["isOutdated"] = serde_json::json!(true);
    moved["comments"]["nodes"][0]["createdAt"] = serde_json::json!("2026-01-01T00:00:05Z");
    github_shows(&gh, "me", serde_json::json!([moved]), serde_json::json!([]));
    let body = json(
        &reviewkit(&state, &remote, &gh)
            .args(["submit", "--session", &id, "--sink", "github"])
            .output()
            .unwrap(),
    );
    assert!(mutation_calls(&gh).is_empty(), "nothing is posted twice");
    assert_eq!(body["data"]["sent"][0]["comment_id"], cid.as_str());
    assert_eq!(body["session"]["pending_submit"], false);
}

#[test]
fn one_post_on_github_vouches_for_only_one_identical_queued_comment() {
    let state = tempfile::tempdir().unwrap();
    let remote = Remote::new();
    let gh = FakeGh::new(&remote, "OPEN", serde_json::json!([]));
    let id = open(&state, &remote, &gh);
    let first = add(
        &state,
        &remote,
        &gh,
        &id,
        serde_json::json!({"path": "keep.txt", "side": "head", "line": 3, "body": "nit: rename"}),
    );
    let second = add(
        &state,
        &remote,
        &gh,
        &id,
        serde_json::json!({"path": "keep.txt", "side": "head", "line": 9, "body": "nit: rename"}),
    );
    pending(
        &state,
        serde_json::json!({
            "at": "2026-01-01T00:00:00Z",
            "comments": [
                { "id": first, "path": "keep.txt", "side": "head", "line": 3, "body": "nit: rename" },
                { "id": second, "path": "keep.txt", "side": "head", "line": 9, "body": "nit: rename" }
            ]
        }),
    );
    github_shows(
        &gh,
        "me",
        serde_json::json!([posted_thread("keep.txt", 3, "me", "nit: rename")]),
        serde_json::json!([]),
    );
    answer_mutation(
        &gh,
        serde_json::json!({ "data": { "review": { "pullRequestReview": { "id": "PRR_9", "url": "u9" } } } }),
        0,
    );
    let body = json(
        &reviewkit(&state, &remote, &gh)
            .args(["submit", "--session", &id, "--sink", "github"])
            .output()
            .unwrap(),
    );
    let sent: Vec<&str> = body["data"]["sent"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["comment_id"].as_str().unwrap())
        .collect();
    assert_eq!(sent.len(), 2);
    let posted = &mutation_calls(&gh)[0]["variables"]["threads"];
    assert_eq!(posted.as_array().unwrap().len(), 1, "the one GitHub lacks is posted");
    assert_eq!(body["session"]["pending_submit"], false);
}

#[test]
fn sending_a_pull_request_review_to_an_agent_keeps_it_for_github() {
    let state = tempfile::tempdir().unwrap();
    let remote = Remote::new();
    let gh = FakeGh::new(
        &remote,
        "OPEN",
        serde_json::json!([thread("keep.txt", Some(3), "RIGHT", false)]),
    );
    let id = open(&state, &remote, &gh);
    let inline = add(
        &state,
        &remote,
        &gh,
        &id,
        serde_json::json!({"path": "keep.txt", "side": "head", "line": 3, "body": "fix this"}),
    );
    add(
        &state,
        &remote,
        &gh,
        &id,
        serde_json::json!({"path": "keep.txt", "side": "head", "line": 3, "body": "agreed", "reply_to": "PRRT_keep.txt"}),
    );
    decide(&state, &remote, &gh, &id, "request_changes", "please");
    let herdr_dir = tempfile::tempdir().unwrap();
    let to_agent = || {
        let mut cmd = reviewkit(&state, &remote, &gh);
        cmd.env("REVIEWKIT_HERDR", common::fixture_bin("herdr"))
            .env("FAKE_HERDR_DIR", herdr_dir.path())
            .args(["submit", "--session", &id, "--sink", "herdr", "--options", "-"])
            .write_stdin(r#"{"target": "w1:p1"}"#)
            .output()
            .unwrap()
    };
    let sent = json(&to_agent());
    assert_eq!(sent["data"]["sent"].as_array().unwrap().len(), 2);
    let prompt = std::fs::read_to_string(herdr_dir.path().join("last_text")).unwrap();
    assert!(prompt.contains("reply to the review thread by bob: \"hm\""), "{prompt}");
    assert_eq!(sent["session"]["comments"].as_array().unwrap().len(), 2);
    assert!(sent["session"]["decision"].is_object());

    let again = json(&to_agent());
    assert_eq!(
        again["data"]["sent"],
        serde_json::json!([]),
        "comments the agent already has are not sent again"
    );

    answer_mutation(
        &gh,
        serde_json::json!({ "data": { "review": { "pullRequestReview": { "id": "PRR_1", "url": "u" } }, "reply0": { "comment": { "id": "C1" } } } }),
        0,
    );
    let posted = json(
        &reviewkit(&state, &remote, &gh)
            .args(["submit", "--session", &id, "--sink", "github"])
            .output()
            .unwrap(),
    );
    assert!(posted["session"]["comments"].as_array().unwrap().is_empty());
    let history = posted["session"]["history"].as_array().unwrap();
    assert!(history.iter().any(|c| c["id"] == inline.as_str()));
}
