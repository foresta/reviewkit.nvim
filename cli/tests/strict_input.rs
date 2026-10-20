mod common;

use common::{Repo, json, reviewkit};

fn run(state: &tempfile::TempDir, repo: &Repo, args: &[&str], stdin: serde_json::Value) -> serde_json::Value {
    let herdr = state.path().join("herdr");
    std::fs::create_dir_all(&herdr).unwrap();
    let out = reviewkit(state, repo)
        .env("REVIEWKIT_HERDR", common::fixture_bin("herdr"))
        .env("FAKE_HERDR_DIR", &herdr)
        .args(args)
        .write_stdin(stdin.to_string())
        .output()
        .unwrap();
    serde_json::from_slice(&out.stdout).unwrap()
}

fn rejected(body: &serde_json::Value, path: &str) {
    assert_eq!(body["error"]["code"], "E_INVALID", "{body}");
    let fields: Vec<&str> = body["error"]["details"]["unknown_fields"]
        .as_array()
        .unwrap_or_else(|| panic!("no unknown_fields: {body}"))
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert!(fields.contains(&path), "{path} not in {fields:?}");
}

#[test]
fn every_input_rejects_keys_it_does_not_know() {
    let state = tempfile::tempdir().unwrap();
    let repo = Repo::new();
    let opened = json(
        &reviewkit(&state, &repo)
            .args(["session", "open", "--local", "--base", "HEAD~1"])
            .output()
            .unwrap(),
    );
    let id = opened["session"]["id"].as_str().unwrap().to_string();
    let added = run(
        &state,
        &repo,
        &["comment", "add", "--session", &id],
        serde_json::json!({"path": "keep.txt", "side": "head", "line": 3, "body": "ok"}),
    );
    let cid = added["data"]["id"].as_str().unwrap().to_string();

    rejected(
        &run(
            &state,
            &repo,
            &["comment", "add", "--session", &id],
            serde_json::json!({"path": "keep.txt", "side": "head", "line": 3, "body": "x", "__x": 1}),
        ),
        "__x",
    );
    rejected(
        &run(
            &state,
            &repo,
            &["comment", "update", "--session", &id, "--id", &cid],
            serde_json::json!({"body": "y", "__x": 1}),
        ),
        "__x",
    );
    rejected(
        &run(
            &state,
            &repo,
            &["decision", "set", "--session", &id],
            serde_json::json!({"event": "comment", "body": "z", "__x": 1}),
        ),
        "__x",
    );
    rejected(
        &run(
            &state,
            &repo,
            &["submit", "--session", &id, "--sink", "herdr", "--options", "-"],
            serde_json::json!({"stag": true}),
        ),
        "stag",
    );
    assert!(
        !state.path().join("herdr/calls.log").exists(),
        "rejected before herdr is reached"
    );
    rejected(
        &run(
            &state,
            &repo,
            &["submit", "--session", &id, "--sink", "github", "--options", "-"],
            serde_json::json!({"__x": 1}),
        ),
        "__x",
    );
    rejected(
        &run(
            &state,
            &repo,
            &[
                "submit",
                "--session",
                &id,
                "--sink",
                "github",
                "--dry-run",
                "--options",
                "-",
            ],
            serde_json::json!({"future_key": 1}),
        ),
        "future_key",
    );
    // A client sink's result arrives after it delivered: refusing it would
    // leave the delivery unrecorded, so extra keys are only logged.
    let applied = run(
        &state,
        &repo,
        &["submit", "apply", "--session", &id],
        serde_json::json!({"sink": "clip", "sent": [], "failed": [], "skipped": [], "decision": "not_applicable", "__x": 1}),
    );
    assert_eq!(applied["ok"], true, "{applied}");
    let unchanged = json(
        &reviewkit(&state, &repo)
            .args(["comment", "list", "--session", &id])
            .output()
            .unwrap(),
    );
    assert_eq!(unchanged["data"][0]["body"], "ok", "nothing was applied");
}

#[test]
fn version_reports_the_contract_revision() {
    let state = tempfile::tempdir().unwrap();
    let repo = Repo::new();
    let body = json(&reviewkit(&state, &repo).arg("version").output().unwrap());
    assert!(body["data"]["schema_revision"].as_u64().is_some(), "{body}");
}
