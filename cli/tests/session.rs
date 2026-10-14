use std::path::{Path, PathBuf};
use std::process::Command as Process;

use assert_cmd::Command;

/// A throwaway repository with two commits, so HEAD~1..HEAD covers a
/// modification, an addition, a deletion and a rename.
struct Repo {
    dir: tempfile::TempDir,
}

impl Repo {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let repo = Self { dir };
        repo.git(&["init", "-q", "-b", "main"]);
        repo.write("keep.txt", "a\nb\nc\nd\ne\nf\ng\nh\ni\nj\n");
        repo.write("old.txt", "one\ntwo\nthree\n");
        repo.write("gone.txt", "gone\n");
        repo.git(&["add", "-A"]);
        repo.git(&["commit", "-q", "-m", "base"]);
        repo.write("keep.txt", "a\nb\nC\nd\ne\nf\ng\nh\ni\nj\nk\n");
        repo.git(&["mv", "old.txt", "new.txt"]);
        repo.git(&["rm", "-q", "gone.txt"]);
        repo.write("added.txt", "new1\nnew2");
        repo.git(&["add", "-A"]);
        repo.git(&["commit", "-q", "-m", "head"]);
        repo
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    fn write(&self, name: &str, content: &str) {
        std::fs::write(self.path().join(name), content).unwrap();
    }

    fn git(&self, args: &[&str]) {
        let status = Process::new(real_git())
            .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
            .args(args)
            .current_dir(self.path())
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed");
    }
}

fn real_git() -> PathBuf {
    which::which("git").expect("git on PATH")
}

fn reviewkit(state: &tempfile::TempDir, repo: &Repo) -> Command {
    let mut cmd = Command::cargo_bin("reviewkit").unwrap();
    cmd.env("REVIEWKIT_GIT", real_git())
        .env("REVIEWKIT_GH", "/nonexistent/gh")
        .env("REVIEWKIT_HERDR", "/nonexistent/herdr")
        .arg("--state-dir")
        .arg(state.path())
        .arg("--cwd")
        .arg(repo.path());
    cmd
}

fn json(output: &std::process::Output) -> serde_json::Value {
    assert!(
        output.status.success(),
        "exit {:?}\nstdout: {}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("stdout is JSON")
}

fn target<'a>(session: &'a serde_json::Value, path: &str) -> &'a serde_json::Value {
    session["targets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["path"] == path)
        .unwrap_or_else(|| panic!("no target {path}"))
}

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
    assert_eq!(resumed["session"]["rev"], 3);
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
