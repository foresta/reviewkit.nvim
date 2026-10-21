use std::path::PathBuf;

use assert_cmd::Command;

fn fixture_bin(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/bin")
        .join(name)
}

fn reviewkit(state_dir: &tempfile::TempDir) -> Command {
    let mut cmd = Command::cargo_bin("reviewkit").expect("binary");
    cmd.env("REVIEWKIT_GIT", fixture_bin("git"))
        .env("REVIEWKIT_GH", fixture_bin("gh"))
        .env("REVIEWKIT_HERDR", fixture_bin("herdr"))
        .env_remove("XDG_STATE_HOME")
        .arg("--state-dir")
        .arg(state_dir.path());
    cmd
}

fn json(output: &std::process::Output) -> serde_json::Value {
    serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|err| panic!("stdout is not JSON: {err}\n{}", String::from_utf8_lossy(&output.stdout)))
}

#[test]
fn version_reports_schema_version() {
    let state = tempfile::tempdir().unwrap();
    let output = reviewkit(&state).arg("version").output().unwrap();
    assert!(output.status.success());
    let body = json(&output);
    assert_eq!(body["ok"], true);
    assert_eq!(body["data"]["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(body["data"]["schema_version"], 1);
    assert!(body.get("session").is_none(), "read-only commands carry no session");
}

#[test]
fn health_reports_fixture_tools() {
    let state = tempfile::tempdir().unwrap();
    let output = reviewkit(&state).arg("health").output().unwrap();
    assert!(output.status.success());
    let body = json(&output);
    insta::assert_json_snapshot!(body, {
        ".data.tools.*.path" => "[path]",
        ".data.state_dir.path" => "[path]",
    });
}

#[test]
fn health_marks_missing_tool_and_unauthenticated_gh() {
    let state = tempfile::tempdir().unwrap();
    let output = reviewkit(&state)
        .env("REVIEWKIT_HERDR", state.path().join("no-such-herdr"))
        .env("GH_FAKE_AUTH", "0")
        .arg("health")
        .output()
        .unwrap();
    assert!(output.status.success(), "health never fails, it reports");
    let body = json(&output);
    assert_eq!(body["data"]["tools"]["herdr"]["version"], serde_json::Value::Null);
    assert_eq!(body["data"]["gh_auth"], false);
    assert_eq!(body["data"]["state_dir"]["writable"], true);
}

#[test]
fn health_runs_gh_with_the_same_pinned_environment_as_every_other_call() {
    let state = tempfile::tempdir().unwrap();
    let gh_dir = tempfile::tempdir().unwrap();
    let output = reviewkit(&state)
        .env("FAKE_GH_DIR", gh_dir.path())
        .env("CLICOLOR_FORCE", "1")
        .env("GH_FORCE_TTY", "1")
        .arg("health")
        .output()
        .unwrap();
    let body = json(&output);
    assert_eq!(
        body["data"]["tools"]["gh"]["version"], "gh version 0.0.0-fake",
        "{body}"
    );
    let calls = std::fs::read_to_string(gh_dir.path().join("calls.jsonl")).unwrap();
    let calls: Vec<serde_json::Value> = calls.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(calls.len(), 2, "--version and auth token: {calls:?}");
    for call in calls {
        assert_eq!(
            call["env"],
            serde_json::json!({ "NO_COLOR": "1", "GH_PROMPT_DISABLED": "1" }),
            "{call}"
        );
    }
}

#[test]
fn schema_lists_contract_types() {
    let state = tempfile::tempdir().unwrap();
    let output = reviewkit(&state).arg("schema").output().unwrap();
    assert!(output.status.success());
    let body = json(&output);
    assert_eq!(body["data"]["schema_version"], 1);
    let defs = &body["data"]["schema"]["$defs"];
    for name in [
        "VersionInfo",
        "HealthInfo",
        "Error",
        "ErrorCode",
        "Session",
        "Target",
        "Comment",
        "SinkResult",
    ] {
        assert!(defs.get(name).is_some(), "missing $defs.{name}");
    }
}

#[test]
fn unknown_subcommand_is_a_usage_error_with_exit_1() {
    let state = tempfile::tempdir().unwrap();
    let output = reviewkit(&state).arg("frobnicate").output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    let body = json(&output);
    assert_eq!(body["ok"], false);
    assert_eq!(body["error"]["code"], "E_USAGE");
    assert_eq!(body["error"]["retryable"], false);
    assert!(body["error"]["hint"].as_str().unwrap().contains("--help"));
}

#[test]
fn missing_cwd_is_invalid_with_exit_2() {
    let state = tempfile::tempdir().unwrap();
    let output = reviewkit(&state)
        .arg("--cwd")
        .arg(state.path().join("nope"))
        .arg("version")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(json(&output)["error"]["code"], "E_INVALID");
}

#[test]
fn help_is_plain_text_and_exit_0() {
    let state = tempfile::tempdir().unwrap();
    let output = reviewkit(&state).arg("--help").output().unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("Usage:"));
}

#[test]
fn no_arguments_is_a_usage_error_with_one_line_message() {
    let output = Command::cargo_bin("reviewkit").unwrap().output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    let body = json(&output);
    assert_eq!(body["error"]["code"], "E_USAGE");
    assert!(!body["error"]["message"].as_str().unwrap().contains('\n'));
    assert!(body["error"]["details"]["usage"].as_str().unwrap().contains("Usage:"));
}

#[test]
fn a_missing_argument_is_named_in_the_usage_message() {
    let state = tempfile::tempdir().unwrap();
    let output = reviewkit(&state).args(["file", "base"]).output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    let message = json(&output)["error"]["message"].as_str().unwrap().to_string();
    assert!(!message.contains('\n'), "{message}");
    assert!(message.contains("--session <ID>"), "{message}");
    assert!(message.contains("--path <PATH>"), "{message}");
}

#[test]
fn empty_tool_override_falls_back_to_path() {
    let state = tempfile::tempdir().unwrap();
    let output = reviewkit(&state)
        .env("REVIEWKIT_GIT", "")
        .env("PATH", fixture_bin("git").parent().unwrap())
        .arg("health")
        .output()
        .unwrap();
    let body = json(&output);
    assert_eq!(
        body["data"]["tools"]["git"]["path"],
        fixture_bin("git").to_str().unwrap()
    );
}

#[test]
fn missing_git_is_a_missing_tool_like_gh_and_herdr() {
    let state = tempfile::tempdir().unwrap();
    let empty = tempfile::tempdir().unwrap();
    let output = reviewkit(&state)
        .env("REVIEWKIT_GIT", "")
        .env("PATH", empty.path())
        .args(["session", "open", "--local"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(4));
    let body = json(&output);
    assert_eq!(body["error"]["code"], "E_TOOL_MISSING");
    assert_eq!(body["error"]["tool"], "git");
}

#[test]
fn state_dir_defaults_follow_the_chain() {
    let state = tempfile::tempdir().unwrap();
    let base = |cmd: &mut Command| {
        cmd.env("REVIEWKIT_GIT", fixture_bin("git"))
            .env("REVIEWKIT_GH", fixture_bin("gh"))
            .env("REVIEWKIT_HERDR", fixture_bin("herdr"))
            .arg("health");
    };

    let mut cmd = Command::cargo_bin("reviewkit").unwrap();
    base(&mut cmd);
    let output = cmd
        .env("XDG_STATE_HOME", state.path())
        .env("REVIEWKIT_STATE_DIR", "")
        .output()
        .unwrap();
    assert_eq!(
        json(&output)["data"]["state_dir"]["path"],
        state.path().join("reviewkit").to_str().unwrap(),
        "empty REVIEWKIT_STATE_DIR is unset; XDG_STATE_HOME/reviewkit is used"
    );

    let mut cmd = Command::cargo_bin("reviewkit").unwrap();
    base(&mut cmd);
    let output = cmd
        .env("XDG_STATE_HOME", state.path())
        .env("REVIEWKIT_STATE_DIR", state.path().join("explicit"))
        .output()
        .unwrap();
    assert_eq!(
        json(&output)["data"]["state_dir"]["path"],
        state.path().join("explicit").to_str().unwrap(),
        "REVIEWKIT_STATE_DIR wins over XDG_STATE_HOME"
    );
}

#[test]
fn debug_logging_keeps_stdout_to_one_json_line() {
    let state = tempfile::tempdir().unwrap();
    let output = reviewkit(&state)
        .env("REVIEWKIT_GIT", state.path().join("missing-git"))
        .args(["--log-level", "debug", "health"])
        .output()
        .unwrap();
    let stdout = String::from_utf8(output.stdout.clone()).unwrap();
    assert_eq!(stdout.trim_end().lines().count(), 1);
    json(&output);
}

#[test]
fn tool_whose_version_fails_has_path_but_no_version() {
    let state = tempfile::tempdir().unwrap();
    let output = reviewkit(&state)
        .env("FAKE_VERSION_FAIL", "1")
        .arg("health")
        .output()
        .unwrap();
    let git = &json(&output)["data"]["tools"]["git"];
    assert!(git["path"].is_string());
    assert!(git["version"].is_null());
}

#[test]
fn empty_log_env_means_default_level() {
    let state = tempfile::tempdir().unwrap();
    let output = reviewkit(&state)
        .env("REVIEWKIT_LOG", "")
        .arg("version")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(json(&output)["ok"], true);
}

#[test]
fn unopenable_log_file_warns_and_still_answers() {
    let state = tempfile::tempdir().unwrap();
    let output = reviewkit(&state)
        .arg("--log-file")
        .arg(state.path().join("no-such-dir").join("log"))
        .arg("version")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(json(&output)["ok"], true);
    assert!(String::from_utf8_lossy(&output.stderr).contains("cannot open --log-file"));
}
