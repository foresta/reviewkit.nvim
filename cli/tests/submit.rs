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
    let preview = &body["data"]["preview"];
    assert_eq!(preview["sink"], "herdr");
    assert_eq!(preview["target"], "w1:p1");
    assert_eq!(preview["mode"], "sent");
    assert_eq!(preview["comment_ids"].as_array().unwrap().len(), 2);
    assert_eq!(preview["decision"], "comment", "a decision with a body is sent");
    assert!(preview["raw"].is_object());
    let prompt = preview["text"].as_str().unwrap();
    assert!(
        prompt.starts_with("Two small things.\n\n1. @keep.txt#L3\nwhy C?\n\n2. @added.txt#L2\ntrailing newline"),
        "{prompt}"
    );
    assert_eq!(
        body["session"]["comments"].as_array().unwrap().len(),
        2,
        "the stored session comes along, unchanged"
    );
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
    let history = json(
        &cmd(&state, &repo, &herdr)
            .args(["history", "list", "--session", &id])
            .output()
            .unwrap(),
    );
    assert_eq!(history["data"].as_array().unwrap().len(), 2);
    assert_eq!(history["data"][0]["deliveries"][1]["mode"], "sent");
}

#[test]
fn a_staged_dry_run_previews_staged_delivery() {
    let state = tempfile::tempdir().unwrap();
    let repo = Repo::new();
    let herdr = FakeHerdr::new(serde_json::json!([]));
    let id = session_with_comments(&state, &repo, &herdr);
    let body = json(
        &cmd(&state, &repo, &herdr)
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
            .write_stdin(r#"{"target": "w9:p3", "stage": true}"#)
            .output()
            .unwrap(),
    );
    assert_eq!(body["data"]["preview"]["mode"], "staged");
    assert_eq!(body["data"]["preview"]["target"], "w9:p3");
}

#[test]
fn a_decision_the_agent_already_received_is_not_sent_again() {
    let state = tempfile::tempdir().unwrap();
    let repo = Repo::new();
    let root = repo.path().canonicalize().unwrap();
    let herdr = FakeHerdr::new(serde_json::json!([agent("w1:p1", root.to_str().unwrap())]));
    let id = session_with_comments(&state, &repo, &herdr);
    let keep = |c: &mut assert_cmd::Command| {
        c.args(["submit", "--session", &id, "--sink", "herdr", "--options", "-"])
            .write_stdin(r#"{"consume": false}"#);
    };
    let mut first = cmd(&state, &repo, &herdr);
    keep(&mut first);
    let sent = json(&first.output().unwrap());
    assert_eq!(sent["data"]["decision"], "sent");
    assert_eq!(sent["session"]["decision"]["deliveries"][0]["sink"], "herdr");
    let mut second = cmd(&state, &repo, &herdr);
    keep(&mut second);
    let out = second.output().unwrap();
    let again: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        again["error"]["code"], "E_INVALID",
        "comments and decision were all delivered: {again}"
    );
}

#[test]
fn a_decision_changed_while_a_lua_sink_ran_is_neither_recorded_nor_cleared() {
    let state = tempfile::tempdir().unwrap();
    let repo = Repo::new();
    let herdr = FakeHerdr::new(serde_json::json!([]));
    let id = session_with_comments(&state, &repo, &herdr);
    let applied = json(
        &cmd(&state, &repo, &herdr)
            .args(["submit", "apply", "--session", &id, "--consume"])
            .write_stdin(
                r#"{"sink": "slack", "sent": [], "failed": [], "skipped": [], "decision": "sent", "decision_version": "an older one"}"#,
            )
            .output()
            .unwrap(),
    );
    assert!(applied["session"]["decision"].is_object());
    assert_eq!(applied["session"]["decision"]["deliveries"], serde_json::json!([]));
}

#[test]
fn the_history_stays_out_of_the_envelope_and_is_listed_on_request() {
    let state = tempfile::tempdir().unwrap();
    let repo = Repo::new();
    let root = repo.path().canonicalize().unwrap();
    let herdr = FakeHerdr::new(serde_json::json!([agent("w1:p1", root.to_str().unwrap())]));
    let id = session_with_comments(&state, &repo, &herdr);
    let sent = json(
        &cmd(&state, &repo, &herdr)
            .args(["submit", "--session", &id, "--sink", "herdr"])
            .output()
            .unwrap(),
    );
    assert!(sent["session"]["comments"].as_array().unwrap().is_empty());
    assert_eq!(
        sent["session"]["history"],
        serde_json::json!([]),
        "every response would carry it"
    );
    let listed = json(
        &cmd(&state, &repo, &herdr)
            .args(["history", "list", "--session", &id])
            .output()
            .unwrap(),
    );
    let history = listed["data"].as_array().unwrap();
    assert_eq!(history.len(), 2);
    assert_eq!(history[0]["deliveries"][0]["sink"], "herdr");
}

#[test]
fn delivered_comments_can_be_cleared_restored_and_the_history_emptied() {
    let state = tempfile::tempdir().unwrap();
    let repo = Repo::new();
    let root = repo.path().canonicalize().unwrap();
    let herdr = FakeHerdr::new(serde_json::json!([agent("w1:p1", root.to_str().unwrap())]));
    let id = session_with_comments(&state, &repo, &herdr);
    let run = |args: &[&str], stdin: Option<&str>| {
        let mut c = cmd(&state, &repo, &herdr);
        c.args(args);
        if let Some(stdin) = stdin {
            c.write_stdin(stdin.to_string());
        }
        let out = c.output().unwrap();
        serde_json::from_slice::<serde_json::Value>(&out.stdout).unwrap()
    };
    let sent = run(
        &["submit", "--session", &id, "--sink", "herdr", "--options", "-"],
        Some(r#"{"consume": false}"#),
    );
    assert_eq!(sent["session"]["comments"].as_array().unwrap().len(), 2);
    let no_flag = run(&["comment", "clear", "--session", &id], None);
    assert_eq!(no_flag["error"]["code"], "E_USAGE", "clearing needs --delivered");
    let other_sink = run(
        &["comment", "clear", "--session", &id, "--delivered", "--sink", "github"],
        None,
    );
    assert_eq!(other_sink["data"]["moved"], serde_json::json!([]));
    let cleared = run(
        &["comment", "clear", "--session", &id, "--delivered", "--sink", "herdr"],
        None,
    );
    let moved = cleared["data"]["moved"].as_array().unwrap().clone();
    assert_eq!(moved.len(), 2);
    assert!(cleared["session"]["comments"].as_array().unwrap().is_empty());
    let cid = moved[0].as_str().unwrap();
    let restored = run(&["history", "restore", "--session", &id, "--id", cid], None);
    let queue = restored["session"]["comments"].as_array().unwrap();
    assert_eq!(queue.len(), 1);
    assert_eq!(queue[0]["deliveries"][0]["sink"], "herdr", "where it went is kept");
    let twice = run(&["history", "restore", "--session", &id, "--id", cid], None);
    assert_eq!(
        twice["session"]["comments"].as_array().unwrap().len(),
        1,
        "restoring is idempotent"
    );
    let missing = run(&["history", "restore", "--session", &id, "--id", "c_nope"], None);
    assert_eq!(missing["error"]["code"], "E_NO_COMMENT");
    let emptied = run(&["history", "clear", "--session", &id], None);
    assert_eq!(emptied["data"]["removed"], 1);
    let listed = run(&["history", "list", "--session", &id], None);
    assert_eq!(listed["data"], serde_json::json!([]));
}

#[test]
fn a_comment_that_reached_its_destination_cannot_come_back() {
    let state = tempfile::tempdir().unwrap();
    let repo = Repo::new();
    let root = repo.path().canonicalize().unwrap();
    let herdr = FakeHerdr::new(serde_json::json!([agent("w1:p1", root.to_str().unwrap())]));
    let id = session_with_comments(&state, &repo, &herdr);
    let sent = json(
        &cmd(&state, &repo, &herdr)
            .args(["submit", "--session", &id, "--sink", "herdr"])
            .output()
            .unwrap(),
    );
    let cid = sent["data"]["sent"][0]["comment_id"].as_str().unwrap().to_string();
    let out = cmd(&state, &repo, &herdr)
        .args(["history", "restore", "--session", &id, "--id", &cid])
        .output()
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        body["error"]["code"], "E_INVALID",
        "editing it would deliver it twice: {body}"
    );
}

#[test]
fn a_selection_sends_only_those_comments_to_the_agent() {
    let state = tempfile::tempdir().unwrap();
    let repo = Repo::new();
    let root = repo.path().canonicalize().unwrap();
    let herdr = FakeHerdr::new(serde_json::json!([agent("w1:p1", root.to_str().unwrap())]));
    let id = session_with_comments(&state, &repo, &herdr);
    let listed = json(
        &cmd(&state, &repo, &herdr)
            .args(["comment", "list", "--session", &id])
            .output()
            .unwrap(),
    );
    let first = listed["data"][0]["id"].as_str().unwrap().to_string();
    let run = |stdin: String| {
        let out = cmd(&state, &repo, &herdr)
            .args(["submit", "--session", &id, "--sink", "herdr", "--options", "-"])
            .write_stdin(stdin)
            .output()
            .unwrap();
        serde_json::from_slice::<serde_json::Value>(&out.stdout).unwrap()
    };
    assert_eq!(run(r#"{"comment_ids": []}"#.into())["error"]["code"], "E_INVALID");
    assert_eq!(
        run(r#"{"comment_ids": ["c_nope"]}"#.into())["error"]["code"],
        "E_NO_COMMENT"
    );
    let sent = run(serde_json::json!({ "comment_ids": [first] }).to_string());
    assert_eq!(sent["data"]["sent"].as_array().unwrap().len(), 1);
    assert_eq!(sent["data"]["decision"], "skipped");
    let prompt = herdr.last_text();
    assert!(
        !prompt.contains("Two small things."),
        "no decision in a selection: {prompt}"
    );
    assert_eq!(sent["session"]["comments"].as_array().unwrap().len(), 1);
    assert!(sent["session"]["decision"].is_object());
    let again = run(serde_json::json!({ "comment_ids": [first] }).to_string());
    assert_eq!(
        again["error"]["code"], "E_INVALID",
        "already in the history, so nothing is left: {again}"
    );
}

#[test]
fn apply_ignores_sent_ids_outside_the_attempt() {
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
    let (a, b) = (
        listed["data"][0]["id"].as_str().unwrap().to_string(),
        listed["data"][1]["id"].as_str().unwrap().to_string(),
    );
    let result = format!(
        r#"{{"sink": "slack", "sent": [{{"comment_id": "{a}"}}, {{"comment_id": "{b}"}}], "failed": [], "skipped": [], "decision": "not_applicable", "attempted": ["{a}"]}}"#
    );
    let applied = json(
        &cmd(&state, &repo, &herdr)
            .args(["submit", "apply", "--session", &id, "--consume"])
            .write_stdin(result)
            .output()
            .unwrap(),
    );
    let left = applied["session"]["comments"].as_array().unwrap();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0]["id"], b.as_str());
}

#[test]
fn a_lua_sink_consumes_only_when_asked() {
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
    let cid = listed["data"][0]["id"].as_str().unwrap().to_string();
    let result = format!(
        r#"{{"sink": "slack", "sent": [{{"comment_id": "{cid}"}}], "failed": [], "skipped": [], "decision": "not_applicable"}}"#
    );
    let kept = json(
        &cmd(&state, &repo, &herdr)
            .args(["submit", "apply", "--session", &id])
            .write_stdin(result.clone())
            .output()
            .unwrap(),
    );
    assert_eq!(kept["session"]["comments"].as_array().unwrap().len(), 2);
    let consumed = json(
        &cmd(&state, &repo, &herdr)
            .args(["submit", "apply", "--session", &id, "--consume"])
            .write_stdin(result)
            .output()
            .unwrap(),
    );
    assert_eq!(consumed["session"]["comments"].as_array().unwrap().len(), 1);
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
    assert_eq!(body["error"]["details"]["candidates"][0]["id"], "a:1");
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
    assert_eq!(body["data"]["failed"][0]["error"]["code"], "E_TOOL");
    assert_eq!(
        body["data"]["failed"][0]["error"]["retryable"], false,
        "the agent may have received it"
    );
    assert_eq!(body["data"]["failed"][0]["error"]["details"]["outcome"], "unknown");
    assert_eq!(body["data"]["decision"], "failed");
    assert_eq!(body["session"]["comments"].as_array().unwrap().len(), 2);
    assert_eq!(body["session"]["comments"][0]["last_error"]["code"], "E_TOOL");
    assert!(body["session"]["decision"].is_object());
}

#[test]
fn a_target_herdr_does_not_know_delivered_nothing() {
    let state = tempfile::tempdir().unwrap();
    let repo = Repo::new();
    let root = repo.path().canonicalize().unwrap();
    let herdr = FakeHerdr::new(serde_json::json!([agent("w1:p1", root.to_str().unwrap())]));
    let id = session_with_comments(&state, &repo, &herdr);
    let body = json(
        &cmd(&state, &repo, &herdr)
            .env("FAKE_HERDR_FAIL", "not_found")
            .args(["submit", "--session", &id, "--sink", "herdr"])
            .output()
            .unwrap(),
    );
    let error = &body["data"]["failed"][0]["error"];
    assert_eq!(error["code"], "E_TOOL", "{error}");
    assert_eq!(error["details"]["not_started"], true, "{error}");
    assert_eq!(error["details"]["herdr_code"], "agent_not_found", "{error}");
    assert!(error["details"].get("outcome").is_none(), "{error}");
    assert_eq!(
        error["message"],
        "herdr agent prompt failed: agent target w1:p1 not found"
    );
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
    let missing = serde_json::from_slice::<serde_json::Value>(&out.stdout).unwrap();
    assert_eq!(missing["error"]["code"], "E_TOOL_MISSING");
    assert_eq!(missing["error"]["tool"], "herdr");
    let bad_options: serde_json::Value = serde_json::from_slice(
        &cmd(&state, &repo, &herdr)
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
            .write_stdin(r#"{"agent_type": "robot"}"#)
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    assert_eq!(bad_options["error"]["code"], "E_INVALID");
    let no_targets: serde_json::Value = serde_json::from_slice(
        &cmd(&state, &repo, &herdr)
            .args(["sink", "targets", "--sink", "github"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    assert_eq!(no_targets["error"]["code"], "E_INVALID");
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
        serde_json::json!({"name": "herdr", "available": true, "reason": null, "runtime": "cli"})
    );
    let targets = json(
        &cmd(&state, &repo, &herdr)
            .args(["sink", "targets", "--sink", "herdr"])
            .output()
            .unwrap(),
    );
    assert_eq!(targets["data"].as_array().unwrap().len(), 1);
    assert_eq!(targets["data"][0]["id"], "w1:p1");
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
    assert_eq!(body["data"]["failed"][0]["error"]["code"], "E_TOOL");
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

#[test]
fn herdr_refuses_a_preview_digest_it_never_gave() {
    let state = tempfile::tempdir().unwrap();
    let repo = Repo::new();
    let herdr = FakeHerdr::new(serde_json::json!([]));
    let id = session_with_comments(&state, &repo, &herdr);
    let out = cmd(&state, &repo, &herdr)
        .args(["submit", "--session", &id, "--sink", "herdr", "--options", "-"])
        .write_stdin(r#"{"target": "w9:p3", "expect_digest": "abc"}"#)
        .output()
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(body["error"]["code"], "E_INVALID");
    assert!(herdr.last_text().is_empty());
}

fn session_dir(state: &tempfile::TempDir, id: &str) -> std::path::PathBuf {
    state.path().join("sessions").join(id)
}

fn comment_ids(state: &tempfile::TempDir, repo: &Repo, herdr: &FakeHerdr, id: &str) -> Vec<String> {
    json(
        &cmd(state, repo, herdr)
            .args(["comment", "list", "--session", id])
            .output()
            .unwrap(),
    )["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["id"].as_str().unwrap().to_string())
        .collect()
}

fn apply_sent(state: &tempfile::TempDir, repo: &Repo, herdr: &FakeHerdr, id: &str, comment: &str) -> serde_json::Value {
    json(
        &cmd(state, repo, herdr)
            .args(["submit", "apply", "--session", id, "--consume"])
            .write_stdin(
                serde_json::json!({ "sink": "slack", "sent": [ { "comment_id": comment } ], "failed": [], "skipped": [], "decision": "not_applicable" })
                    .to_string(),
            )
            .output()
            .unwrap(),
    )
}

#[test]
fn a_lua_sink_result_waits_out_a_held_session_and_is_recorded_by_the_next_write() {
    let state = tempfile::tempdir().unwrap();
    let repo = Repo::new();
    let herdr = FakeHerdr::new(serde_json::json!([]));
    let id = session_with_comments(&state, &repo, &herdr);
    let ids = comment_ids(&state, &repo, &herdr, &id);
    let lock_file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(session_dir(&state, &id).join("session.lock"))
        .unwrap();
    let mut lock = fd_lock::RwLock::new(lock_file);
    let held = lock.write().unwrap();

    let queued = apply_sent(&state, &repo, &herdr, &id, &ids[0]);
    assert_eq!(queued["ok"], true, "{queued}");
    assert_eq!(queued["session"]["pending_apply"], true);
    assert_eq!(queued["session"]["pending_apply_newer"], false);
    assert_eq!(queued["session"]["comments"].as_array().unwrap().len(), 2);

    drop(held);
    let listed = json(
        &cmd(&state, &repo, &herdr)
            .args(["session", "get", "--session", &id])
            .output()
            .unwrap(),
    );
    assert_eq!(listed["session"]["pending_apply"], true, "a read records nothing");
    let decided = json(
        &cmd(&state, &repo, &herdr)
            .args(["decision", "set", "--session", &id])
            .write_stdin(r#"{"event": "approve", "body": ""}"#)
            .output()
            .unwrap(),
    );
    assert_eq!(decided["session"]["pending_apply"], false);
    assert_eq!(comment_ids(&state, &repo, &herdr, &id), vec![ids[1].clone()]);
    let history = json(
        &cmd(&state, &repo, &herdr)
            .args(["history", "list", "--session", &id])
            .output()
            .unwrap(),
    );
    assert_eq!(history["data"][0]["deliveries"].as_array().unwrap().len(), 1);
}

#[test]
fn a_waiting_result_is_recorded_on_request_and_a_broken_one_is_set_aside() {
    let state = tempfile::tempdir().unwrap();
    let repo = Repo::new();
    let herdr = FakeHerdr::new(serde_json::json!([]));
    let id = session_with_comments(&state, &repo, &herdr);
    let ids = comment_ids(&state, &repo, &herdr, &id);
    let lock_file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(session_dir(&state, &id).join("session.lock"))
        .unwrap();
    let mut lock = fd_lock::RwLock::new(lock_file);
    let held = lock.write().unwrap();
    apply_sent(&state, &repo, &herdr, &id, &ids[1]);
    std::fs::write(session_dir(&state, &id).join("applies/0-broken.json"), "{").unwrap();
    drop(held);

    let drained = json(
        &cmd(&state, &repo, &herdr)
            .args(["submit", "apply", "--session", &id, "--drain"])
            .output()
            .unwrap(),
    );
    assert_eq!(drained["ok"], true, "{drained}");
    assert_eq!(drained["session"]["pending_apply"], false);
    assert_eq!(comment_ids(&state, &repo, &herdr, &id), vec![ids[0].clone()]);
    assert!(session_dir(&state, &id).join("applies/rejected/0-broken.json").exists());
}

#[test]
fn a_result_queued_by_a_newer_cli_stays_waiting_and_is_left_for_it() {
    let state = tempfile::tempdir().unwrap();
    let repo = Repo::new();
    let herdr = FakeHerdr::new(serde_json::json!([]));
    let id = session_with_comments(&state, &repo, &herdr);
    let applies = session_dir(&state, &id).join("applies");
    std::fs::create_dir_all(&applies).unwrap();
    std::fs::write(
        applies.join("01ZZZZZZZZZZZZZZZZZZZZZZZZ.json"),
        r#"{"revision": 999999, "consume": true, "result": {"sink": "slack", "sent": [], "failed": [], "skipped": [], "decision": "not_applicable"}}"#,
    )
    .unwrap();
    let got = json(
        &cmd(&state, &repo, &herdr)
            .args(["session", "get", "--session", &id])
            .output()
            .unwrap(),
    );
    assert_eq!(
        got["session"]["pending_apply"], true,
        "another instance's delivery is still unrecorded"
    );
    assert_eq!(
        got["session"]["pending_apply_newer"], true,
        "only a newer CLI can record it"
    );
    let decided = json(
        &cmd(&state, &repo, &herdr)
            .args(["decision", "set", "--session", &id])
            .write_stdin(r#"{"event": "approve", "body": ""}"#)
            .output()
            .unwrap(),
    );
    assert_eq!(decided["ok"], true);
    assert!(
        applies.join("01ZZZZZZZZZZZZZZZZZZZZZZZZ.json").exists(),
        "left for the newer CLI"
    );
}

#[test]
fn a_result_a_newer_cli_queued_with_values_this_one_does_not_know_is_left_alone() {
    let state = tempfile::tempdir().unwrap();
    let repo = Repo::new();
    let herdr = FakeHerdr::new(serde_json::json!([]));
    let id = session_with_comments(&state, &repo, &herdr);
    let applies = session_dir(&state, &id).join("applies");
    std::fs::create_dir_all(&applies).unwrap();
    let file = applies.join("01ZZZZZZZZZZZZZZZZZZZZZZZZ.json");
    std::fs::write(
        &file,
        r#"{"revision": 999999, "consume": true, "result": {"sink": "slack", "mode": "beamed", "sent": [], "failed": [], "skipped": [], "decision": "not_applicable"}}"#,
    )
    .unwrap();
    let decided = json(
        &cmd(&state, &repo, &herdr)
            .args(["decision", "set", "--session", &id])
            .write_stdin(r#"{"event": "approve", "body": ""}"#)
            .output()
            .unwrap(),
    );
    assert_eq!(decided["ok"], true, "{decided}");
    assert!(file.exists(), "not set aside: only the newer CLI can read it");
    assert_eq!(decided["session"]["pending_apply"], true);
    assert_eq!(decided["session"]["pending_apply_newer"], true);
}

fn queued_ids(state: &tempfile::TempDir, repo: &Repo, herdr: &FakeHerdr, id: &str) -> (String, String) {
    let listed = json(
        &cmd(state, repo, herdr)
            .args(["comment", "list", "--session", id])
            .output()
            .unwrap(),
    );
    (
        listed["data"][0]["id"].as_str().unwrap().to_string(),
        listed["data"][1]["id"].as_str().unwrap().to_string(),
    )
}

#[test]
fn submit_apply_records_sent_comments_beside_an_unknown_error_code() {
    let state = tempfile::tempdir().unwrap();
    let repo = Repo::new();
    let herdr = FakeHerdr::new(serde_json::json!([]));
    let id = session_with_comments(&state, &repo, &herdr);
    let (first, second) = queued_ids(&state, &repo, &herdr, &id);
    let result = serde_json::json!({
        "sink": "custom",
        "sent": [{"comment_id": first}],
        "failed": [
            {"comment_id": second, "error": {"code": "E_CLIPBOARD", "message": "register busy", "details": {"register": "+"}}},
            {"comment_id": null, "error": {"message": "no code at all"}},
        ],
        "skipped": [],
        "decision": "not_applicable"
    });
    let body = json(
        &cmd(&state, &repo, &herdr)
            .args(["submit", "apply", "--session", &id, "--consume"])
            .write_stdin(result.to_string())
            .output()
            .unwrap(),
    );
    assert_eq!(body["ok"], true, "{body}");
    let comments = body["session"]["comments"].as_array().unwrap();
    assert_eq!(comments.len(), 1, "the delivered comment leaves the queue: {body}");
    assert_eq!(comments[0]["id"], second);
    assert_eq!(comments[0]["last_error"]["code"], "E_TOOL");
    assert!(
        comments[0]["last_error"]["message"]
            .as_str()
            .unwrap()
            .contains("E_CLIPBOARD"),
        "{body}"
    );
    let failed = &body["data"]["failed"];
    assert_eq!(failed[0]["error"]["details"]["code"], "E_CLIPBOARD");
    assert_eq!(failed[0]["error"]["details"]["register"], "+");
    assert_eq!(failed[1]["error"]["code"], "E_TOOL");
    assert_eq!(body["session"]["pending_apply"], false);
}

#[test]
fn submit_apply_degrades_unknown_reasons_modes_and_statuses() {
    let state = tempfile::tempdir().unwrap();
    let repo = Repo::new();
    let herdr = FakeHerdr::new(serde_json::json!([]));
    let id = session_with_comments(&state, &repo, &herdr);
    let (first, second) = queued_ids(&state, &repo, &herdr, &id);
    let result = serde_json::json!({
        "sink": "custom",
        "sent": [{"comment_id": first}],
        "failed": [],
        "skipped": [{"comment_id": second, "reason": "muted_by_user"}],
        "thread_actions": [
            {"thread_id": "T_1", "action": "resolve", "status": "queued_remotely"},
            {"thread_id": "T_2", "action": "pin", "status": "sent"},
        ],
        "decision": "drafted",
        "mode": "printed"
    });
    let body = json(
        &cmd(&state, &repo, &herdr)
            .args(["submit", "apply", "--session", &id, "--consume"])
            .write_stdin(result.to_string())
            .output()
            .unwrap(),
    );
    assert_eq!(body["ok"], true, "{body}");
    let data = &body["data"];
    assert_eq!(data["skipped"][0]["reason"], "not_supported");
    assert_eq!(data["mode"], "copied");
    assert_eq!(data["decision"], "not_applicable");
    assert_eq!(data["thread_actions"].as_array().unwrap().len(), 1, "{data}");
    assert_eq!(data["thread_actions"][0]["status"], "failed");
    let comments = body["session"]["comments"].as_array().unwrap();
    assert_eq!(comments.len(), 2, "an unknown mode never consumes: {body}");
    assert_eq!(comments[0]["deliveries"][0]["sink"], "custom");
    assert_eq!(comments[0]["deliveries"][0]["mode"], "copied");
    assert!(
        body["session"]["decision"].is_object(),
        "an unknown decision status keeps the decision"
    );
}
