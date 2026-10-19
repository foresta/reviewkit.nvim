mod common;

use common::{Repo, json, reviewkit};

struct FakeHerdr {
    dir: tempfile::TempDir,
}

impl FakeHerdr {
    fn new(agents: serde_json::Value) -> Self {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("agents.json"),
            serde_json::json!({"result": {"agents": agents}}).to_string(),
        )
        .unwrap();
        std::fs::write(
            dir.path().join("panes.json"),
            serde_json::json!({"result": {"panes": agents}}).to_string(),
        )
        .unwrap();
        Self { dir }
    }

    fn last_text(&self) -> String {
        std::fs::read_to_string(self.dir.path().join("last_text")).unwrap_or_default()
    }

    fn last_target(&self) -> String {
        std::fs::read_to_string(self.dir.path().join("last_target")).unwrap_or_default()
    }

    fn calls(&self) -> String {
        std::fs::read_to_string(self.dir.path().join("calls.log")).unwrap_or_default()
    }
}

fn agent(pane: &str, cwd: &str) -> serde_json::Value {
    serde_json::json!({"pane_id": pane, "agent": "claude", "agent_status": "idle", "cwd": cwd, "foreground_cwd": cwd, "terminal_title_stripped": "t"})
}

fn cmd(state: &tempfile::TempDir, repo: &Repo, herdr: &FakeHerdr) -> assert_cmd::Command {
    let mut c = reviewkit(state, repo);
    c.env("REVIEWKIT_HERDR", common::fixture_bin("herdr"))
        .env("FAKE_HERDR_DIR", herdr.dir.path());
    c
}

fn session_with_comments(state: &tempfile::TempDir, repo: &Repo, herdr: &FakeHerdr) -> String {
    let opened = json(
        &cmd(state, repo, herdr)
            .args(["session", "open", "--local", "--base", "HEAD~1"])
            .output()
            .unwrap(),
    );
    let id = opened["session"]["id"].as_str().unwrap().to_string();
    for (path, line, body) in [("keep.txt", 3, "why C?"), ("added.txt", 2, "trailing newline")] {
        let input = serde_json::json!({"path": path, "side": "head", "line": line, "body": body});
        json(
            &cmd(state, repo, herdr)
                .args(["comment", "add", "--session", &id])
                .write_stdin(input.to_string())
                .output()
                .unwrap(),
        );
    }
    json(
        &cmd(state, repo, herdr)
            .args(["decision", "set", "--session", &id])
            .write_stdin(r#"{"event": "comment", "body": "Two small things."}"#)
            .output()
            .unwrap(),
    );
    id
}

#[test]
fn dry_run_previews_the_prompt_and_resolves_the_target_by_cwd() {
    let state = tempfile::tempdir().unwrap();
    let repo = Repo::new();
    let root = repo.path().canonicalize().unwrap();
    let herdr = FakeHerdr::new(serde_json::json!([
        agent("w1:p1", root.to_str().unwrap()),
        agent("w2:p1", "/elsewhere")
    ]));
    let id = session_with_comments(&state, &repo, &herdr);
    let body = json(
        &cmd(&state, &repo, &herdr)
            .args(["submit", "--session", &id, "--sink", "herdr", "--dry-run"])
            .output()
            .unwrap(),
    );
    assert_eq!(body["data"]["preview"]["target"], "w1:p1");
    let prompt = body["data"]["preview"]["prompt"].as_str().unwrap();
    assert!(
        prompt.starts_with("Two small things.\n\n1. @keep.txt#L3\nwhy C?\n\n2. @added.txt#L2\ntrailing newline"),
        "{prompt}"
    );
    assert!(body.get("session").is_none());
    assert!(herdr.last_text().is_empty(), "dry run sends nothing");
}

#[test]
fn submit_sends_one_prompt_and_clears_the_queue_and_decision() {
    let state = tempfile::tempdir().unwrap();
    let repo = Repo::new();
    let root = repo.path().canonicalize().unwrap();
    let herdr = FakeHerdr::new(serde_json::json!([agent("w1:p1", root.to_str().unwrap())]));
    let id = session_with_comments(&state, &repo, &herdr);
    let body = json(
        &cmd(&state, &repo, &herdr)
            .args(["submit", "--session", &id, "--sink", "herdr", "--options", "-"])
            .write_stdin(r#"{"header": "Review notes:"}"#)
            .output()
            .unwrap(),
    );
    assert_eq!(body["data"]["sent"].as_array().unwrap().len(), 2);
    assert_eq!(body["data"]["decision"], "sent");
    assert_eq!(body["data"]["summary"], "2 sent, 0 failed, decision sent");
    assert!(body["session"]["comments"].as_array().unwrap().is_empty());
    assert_eq!(body["session"]["decision"], serde_json::Value::Null);
    assert_eq!(body["session"]["last_submit"]["sink"], "herdr");
    assert_eq!(herdr.last_target(), "w1:p1");
    assert!(herdr.last_text().starts_with("Review notes:\n\nTwo small things."));
    assert!(herdr.calls().contains("agent prompt w1:p1"));
}

#[test]
fn stage_uses_send_text_and_explicit_target_skips_resolution() {
    let state = tempfile::tempdir().unwrap();
    let repo = Repo::new();
    let herdr = FakeHerdr::new(serde_json::json!([]));
    let id = session_with_comments(&state, &repo, &herdr);
    let body = json(
        &cmd(&state, &repo, &herdr)
            .args(["submit", "--session", &id, "--sink", "herdr", "--options", "-"])
            .write_stdin(r#"{"target": "w9:p3", "stage": true, "agent_type": "plain"}"#)
            .output()
            .unwrap(),
    );
    assert_eq!(body["data"]["sent"].as_array().unwrap().len(), 2);
    assert!(herdr.calls().contains("pane send-text w9:p3"));
    assert!(herdr.last_text().contains("1. keep.txt:3\n"));
    let comments = body["session"]["comments"].as_array().unwrap();
    assert_eq!(comments.len(), 2, "staged text may be discarded in the agent's input");
    assert_eq!(comments[0]["deliveries"][0]["mode"], "staged");
    assert!(body["session"]["decision"].is_object());

    let sent = json(
        &cmd(&state, &repo, &herdr)
            .args(["submit", "--session", &id, "--sink", "herdr", "--options", "-"])
            .write_stdin(r#"{"target": "w9:p3", "agent_type": "plain"}"#)
            .output()
            .unwrap(),
    );
    assert_eq!(
        sent["data"]["sent"].as_array().unwrap().len(),
        2,
        "staged ones are sent for real"
    );
    assert!(sent["session"]["comments"].as_array().unwrap().is_empty());
    assert_eq!(sent["session"]["history"].as_array().unwrap().len(), 2);
    assert_eq!(sent["session"]["history"][0]["deliveries"][1]["mode"], "sent");
}

#[test]
fn editing_a_delivered_comment_forgets_where_it_went() {
    let state = tempfile::tempdir().unwrap();
    let repo = Repo::new();
    let herdr = FakeHerdr::new(serde_json::json!([]));
    let id = session_with_comments(&state, &repo, &herdr);
    let staged = json(
        &cmd(&state, &repo, &herdr)
            .args(["submit", "--session", &id, "--sink", "herdr", "--options", "-"])
            .write_stdin(r#"{"target": "w9:p3", "stage": true}"#)
            .output()
            .unwrap(),
    );
    let cid = staged["session"]["comments"][0]["id"].as_str().unwrap().to_string();
    let edited = json(
        &cmd(&state, &repo, &herdr)
            .args(["comment", "update", "--session", &id, "--id", &cid])
            .write_stdin(r#"{"body": "rewritten"}"#)
            .output()
            .unwrap(),
    );
    assert_eq!(edited["session"]["comments"][0]["deliveries"], serde_json::json!([]));
}

#[test]
fn no_matching_agent_is_e_ambiguous_with_candidates() {
    let state = tempfile::tempdir().unwrap();
    let repo = Repo::new();
    let herdr = FakeHerdr::new(serde_json::json!([agent("a:1", "/x"), agent("b:1", "/y")]));
    let id = session_with_comments(&state, &repo, &herdr);
    let out = cmd(&state, &repo, &herdr)
        .args(["submit", "--session", &id, "--sink", "herdr"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(4));
    let body: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(body["error"]["code"], "E_AMBIGUOUS");
    assert_eq!(body["error"]["details"]["candidates"].as_array().unwrap().len(), 2);
    assert_eq!(body["error"]["details"]["candidates"][0]["pane_id"], "a:1");
}

#[test]
fn a_failed_send_keeps_the_queue_with_last_error() {
    let state = tempfile::tempdir().unwrap();
    let repo = Repo::new();
    let root = repo.path().canonicalize().unwrap();
    let herdr = FakeHerdr::new(serde_json::json!([agent("w1:p1", root.to_str().unwrap())]));
    let id = session_with_comments(&state, &repo, &herdr);
    let body = json(
        &cmd(&state, &repo, &herdr)
            .env("FAKE_HERDR_FAIL", "1")
            .args(["submit", "--session", &id, "--sink", "herdr"])
            .output()
            .unwrap(),
    );
    assert_eq!(body["data"]["failed"][0]["comment_id"], serde_json::Value::Null);
    assert_eq!(body["data"]["failed"][0]["error"]["code"], "E_HERDR");
    assert_eq!(body["data"]["decision"], "failed");
    assert_eq!(body["session"]["comments"].as_array().unwrap().len(), 2);
    assert_eq!(body["session"]["comments"][0]["last_error"]["code"], "E_HERDR");
    assert!(body["session"]["decision"].is_object());
}

#[test]
fn empty_queue_and_missing_herdr_fail_before_sending() {
    let state = tempfile::tempdir().unwrap();
    let repo = Repo::new();
    let herdr = FakeHerdr::new(serde_json::json!([]));
    let opened = json(
        &cmd(&state, &repo, &herdr)
            .args(["session", "open", "--local", "--base", "HEAD~1"])
            .output()
            .unwrap(),
    );
    let id = opened["session"]["id"].as_str().unwrap().to_string();
    let out = cmd(&state, &repo, &herdr)
        .args(["submit", "--session", &id, "--sink", "herdr"])
        .output()
        .unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&out.stdout).unwrap()["error"]["code"],
        "E_INVALID"
    );
    assert!(herdr.calls().is_empty(), "an empty queue never touches herdr");

    let id = session_with_comments(&state, &repo, &herdr);
    let out = cmd(&state, &repo, &herdr)
        .env("REVIEWKIT_HERDR", state.path().join("no-herdr"))
        .args(["submit", "--session", &id, "--sink", "herdr"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(4));
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&out.stdout).unwrap()["error"]["code"],
        "E_HERDR_MISSING"
    );
    let out = cmd(&state, &repo, &herdr)
        .args(["submit", "--session", &id, "--sink", "nope"])
        .output()
        .unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&out.stdout).unwrap()["error"]["code"],
        "E_INVALID"
    );
}

#[test]
fn sink_list_and_targets() {
    let state = tempfile::tempdir().unwrap();
    let repo = Repo::new();
    let herdr = FakeHerdr::new(serde_json::json!([agent("w1:p1", "/r")]));
    let list = json(&cmd(&state, &repo, &herdr).args(["sink", "list"]).output().unwrap());
    assert_eq!(
        list["data"][0],
        serde_json::json!({"name": "herdr", "available": true, "reason": null})
    );
    let targets = json(
        &cmd(&state, &repo, &herdr)
            .args(["sink", "targets", "--sink", "herdr"])
            .output()
            .unwrap(),
    );
    assert_eq!(targets["data"]["agents"][0]["pane_id"], "w1:p1");
    assert_eq!(targets["data"]["panes"].as_array().unwrap().len(), 1);
}

#[test]
fn submit_apply_records_a_client_side_result() {
    let state = tempfile::tempdir().unwrap();
    let repo = Repo::new();
    let herdr = FakeHerdr::new(serde_json::json!([]));
    let id = session_with_comments(&state, &repo, &herdr);
    let listed = json(
        &cmd(&state, &repo, &herdr)
            .args(["comment", "list", "--session", &id])
            .output()
            .unwrap(),
    );
    let first = listed["data"][0]["id"].as_str().unwrap().to_string();
    let second = listed["data"][1]["id"].as_str().unwrap().to_string();
    let result = serde_json::json!({
        "sink": "clipboard",
        "sent": [{"comment_id": first}],
        "failed": [{"comment_id": second, "error": {"code": "E_INTERNAL", "message": "clipboard full"}}],
        "skipped": [],
        "decision": "skipped"
    });
    let body = json(
        &cmd(&state, &repo, &herdr)
            .args(["submit", "apply", "--session", &id])
            .write_stdin(result.to_string())
            .output()
            .unwrap(),
    );
    assert_eq!(body["data"]["summary"], "1 sent, 1 failed, decision skipped");
    assert!(!body["data"]["at"].as_str().unwrap().is_empty());
    let comments = body["session"]["comments"].as_array().unwrap();
    assert_eq!(
        comments.len(),
        2,
        "a client-side sink is not a local review's destination"
    );
    assert_eq!(comments[0]["id"], first);
    assert_eq!(comments[0]["deliveries"][0]["sink"], "clipboard");
    assert_eq!(comments[1]["last_error"]["sink"], "clipboard");
    assert!(body["session"]["decision"].is_object(), "skipped decisions stay");
    assert_eq!(body["session"]["last_submit"]["result"]["sink"], "clipboard");
}

#[test]
fn a_whole_review_failure_leaves_skipped_comments_untouched() {
    let state = tempfile::tempdir().unwrap();
    let repo = Repo::new();
    let herdr = FakeHerdr::new(serde_json::json!([]));
    let id = session_with_comments(&state, &repo, &herdr);
    let listed = json(
        &cmd(&state, &repo, &herdr)
            .args(["comment", "list", "--session", &id])
            .output()
            .unwrap(),
    );
    let skipped_id = listed["data"][1]["id"].as_str().unwrap().to_string();
    let result = serde_json::json!({
        "sink": "clipboard",
        "sent": [],
        "failed": [{"comment_id": null, "error": {"code": "E_INTERNAL", "message": "nope"}}],
        "skipped": [{"comment_id": skipped_id, "reason": "outside_diff"}],
        "decision": "failed"
    });
    let body = json(
        &cmd(&state, &repo, &herdr)
            .args(["submit", "apply", "--session", &id])
            .write_stdin(result.to_string())
            .output()
            .unwrap(),
    );
    let comments = body["session"]["comments"].as_array().unwrap();
    assert_eq!(comments.len(), 2);
    assert_eq!(comments[0]["last_error"]["code"], "E_INTERNAL");
    assert!(comments[1]["last_error"].is_null(), "skipped comments carry no error");
}

#[test]
fn decision_body_alone_is_sent_and_a_bare_event_is_skipped() {
    let state = tempfile::tempdir().unwrap();
    let repo = Repo::new();
    let root = repo.path().canonicalize().unwrap();
    let herdr = FakeHerdr::new(serde_json::json!([agent("w1:p1", root.to_str().unwrap())]));
    let opened = json(
        &cmd(&state, &repo, &herdr)
            .args(["session", "open", "--local", "--base", "HEAD~1"])
            .output()
            .unwrap(),
    );
    let id = opened["session"]["id"].as_str().unwrap().to_string();
    json(
        &cmd(&state, &repo, &herdr)
            .args(["decision", "set", "--session", &id])
            .write_stdin(r#"{"event": "approve", "body": "Looks good overall."}"#)
            .output()
            .unwrap(),
    );
    let body = json(
        &cmd(&state, &repo, &herdr)
            .args(["submit", "--session", &id, "--sink", "herdr"])
            .output()
            .unwrap(),
    );
    assert!(body["data"]["sent"].as_array().unwrap().is_empty());
    assert_eq!(body["data"]["decision"], "sent");
    assert!(body["session"]["decision"].is_null());
    assert_eq!(herdr.last_text(), "Looks good overall.");

    let id = session_with_comments(&state, &repo, &herdr);
    json(
        &cmd(&state, &repo, &herdr)
            .args(["decision", "set", "--session", &id])
            .write_stdin(r#"{"event": "approve"}"#)
            .output()
            .unwrap(),
    );
    let body = json(
        &cmd(&state, &repo, &herdr)
            .args(["submit", "--session", &id, "--sink", "herdr"])
            .output()
            .unwrap(),
    );
    assert_eq!(body["data"]["decision"], "skipped");
    assert_eq!(
        body["session"]["decision"]["event"], "approve",
        "an event without text cannot reach an agent"
    );
    assert_eq!(body["data"]["summary"], "2 sent, 0 failed, decision skipped");
}

#[test]
fn foreground_cwd_matching_can_be_turned_off_through_options() {
    let state = tempfile::tempdir().unwrap();
    let repo = Repo::new();
    let root = repo.path().canonicalize().unwrap();
    let mut only_foreground = agent("w1:p1", "/elsewhere");
    only_foreground["foreground_cwd"] = serde_json::json!(root.to_str().unwrap());
    let herdr = FakeHerdr::new(serde_json::json!([only_foreground]));
    let id = session_with_comments(&state, &repo, &herdr);
    let by_default = json(
        &cmd(&state, &repo, &herdr)
            .args(["submit", "--session", &id, "--sink", "herdr", "--dry-run"])
            .output()
            .unwrap(),
    );
    assert_eq!(by_default["data"]["preview"]["target"], "w1:p1");
    let out = cmd(&state, &repo, &herdr)
        .args([
            "submit",
            "--session",
            &id,
            "--sink",
            "herdr",
            "--dry-run",
            "--options",
            "-",
        ])
        .write_stdin(r#"{"match_foreground_cwd": false}"#)
        .output()
        .unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&out.stdout).unwrap()["error"]["code"],
        "E_AMBIGUOUS"
    );
}

#[test]
fn submit_apply_rejects_bad_input_and_a_hanging_herdr_times_out() {
    let state = tempfile::tempdir().unwrap();
    let repo = Repo::new();
    let root = repo.path().canonicalize().unwrap();
    let herdr = FakeHerdr::new(serde_json::json!([agent("w1:p1", root.to_str().unwrap())]));
    let id = session_with_comments(&state, &repo, &herdr);
    let out = cmd(&state, &repo, &herdr)
        .args(["submit", "apply", "--session", &id])
        .write_stdin("{ nope")
        .output()
        .unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&out.stdout).unwrap()["error"]["code"],
        "E_INVALID"
    );
    let out = cmd(&state, &repo, &herdr)
        .args(["submit", "apply"])
        .write_stdin("{}")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));

    let body = json(
        &cmd(&state, &repo, &herdr)
            .env("FAKE_HERDR_SLEEP", "3")
            .env("REVIEWKIT_HERDR_TIMEOUT_MS", "1000")
            .args(["submit", "--session", &id, "--sink", "herdr"])
            .output()
            .unwrap(),
    );
    assert_eq!(body["data"]["failed"][0]["error"]["code"], "E_HERDR");
    assert!(
        body["data"]["failed"][0]["error"]["message"]
            .as_str()
            .unwrap()
            .contains("did not finish")
    );
    assert!(
        !body["data"]["failed"][0]["error"]["message"]
            .as_str()
            .unwrap()
            .contains("why C?"),
        "the prompt text never leaks into errors"
    );
    assert_eq!(body["session"]["comments"].as_array().unwrap().len(), 2);
}

#[test]
fn a_later_success_clears_the_failure_mark_and_a_failure_spares_comments_already_delivered() {
    let state = tempfile::tempdir().unwrap();
    let repo = Repo::new();
    let herdr = FakeHerdr::new(serde_json::json!([]));
    let id = session_with_comments(&state, &repo, &herdr);
    let send = |fail: bool| {
        let mut c = cmd(&state, &repo, &herdr);
        if fail {
            c.env("FAKE_HERDR_FAIL", "1");
        }
        json(
            &c.args(["submit", "--session", &id, "--sink", "herdr", "--options", "-"])
                .write_stdin(r#"{"target": "w9:p3", "consume": false}"#)
                .output()
                .unwrap(),
        )
    };
    let failed = send(true);
    assert!(failed["session"]["comments"][0]["last_error"].is_object());
    let ok = send(false);
    assert!(ok["session"]["comments"][0]["last_error"].is_null());

    json(
        &cmd(&state, &repo, &herdr)
            .args(["comment", "add", "--session", &id])
            .write_stdin(r#"{"path": "keep.txt", "side": "head", "line": 3, "body": "one more"}"#)
            .output()
            .unwrap(),
    );
    let again = send(true);
    let comments = again["session"]["comments"].as_array().unwrap();
    assert!(comments[0]["last_error"].is_null(), "already with the agent");
    assert!(comments[2]["last_error"].is_object());
}

#[test]
fn a_comment_edited_while_a_client_side_sink_ran_is_not_taken_as_delivered() {
    let state = tempfile::tempdir().unwrap();
    let repo = Repo::new();
    let herdr = FakeHerdr::new(serde_json::json!([]));
    let id = session_with_comments(&state, &repo, &herdr);
    let listed = json(
        &cmd(&state, &repo, &herdr)
            .args(["comment", "list", "--session", &id])
            .output()
            .unwrap(),
    );
    let first = listed["data"][0]["id"].as_str().unwrap().to_string();
    let seen = listed["data"][0]["updated_at"].as_str().unwrap().to_string();
    let second = listed["data"][1]["id"].as_str().unwrap().to_string();
    json(
        &cmd(&state, &repo, &herdr)
            .args(["comment", "update", "--session", &id, "--id", &first])
            .write_stdin(r#"{"body": "edited meanwhile"}"#)
            .output()
            .unwrap(),
    );
    let added = json(
        &cmd(&state, &repo, &herdr)
            .args(["comment", "add", "--session", &id])
            .write_stdin(r#"{"path": "keep.txt", "side": "head", "line": 3, "body": "added meanwhile"}"#)
            .output()
            .unwrap(),
    );
    let third = added["data"]["id"].as_str().unwrap().to_string();
    let result = serde_json::json!({
        "sink": "capture",
        "sent": [{"comment_id": first, "version": seen}],
        "failed": [],
        "skipped": [],
        "decision": "not_applicable"
    });
    let body = json(
        &cmd(&state, &repo, &herdr)
            .args(["submit", "apply", "--session", &id])
            .write_stdin(result.to_string())
            .output()
            .unwrap(),
    );
    let comments = body["session"]["comments"].as_array().unwrap();
    let first_now = comments.iter().find(|c| c["id"] == first.as_str()).unwrap();
    assert_eq!(
        first_now["deliveries"],
        serde_json::json!([]),
        "the edited text was never sent"
    );

    let whole_failure = serde_json::json!({
        "sink": "capture",
        "sent": [],
        "failed": [{"comment_id": null, "error": {"code": "E_INTERNAL", "message": "broken"}}],
        "skipped": [],
        "decision": "not_applicable",
        "attempted": [second]
    });
    let failed = json(
        &cmd(&state, &repo, &herdr)
            .args(["submit", "apply", "--session", &id])
            .write_stdin(whole_failure.to_string())
            .output()
            .unwrap(),
    );
    let comments = failed["session"]["comments"].as_array().unwrap();
    let marked = |cid: &str| comments.iter().find(|c| c["id"] == cid).unwrap()["last_error"].is_object();
    assert!(marked(&second));
    assert!(!marked(&third), "added after the attempt started");
}
