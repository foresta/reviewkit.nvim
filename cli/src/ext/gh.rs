use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::Deserialize;

use crate::error::{Error, ErrorCode, Result};
use crate::model::session::Side;
use crate::model::{Check, PrInfo, PrState, SubjectType, Thread, ThreadComment, TimelineItem, TimelineKind};
use crate::store::env_non_empty;

/// The seam for tests: source-pr and sink-github talk to this trait, the
/// binary talks to `SystemGh`.
pub trait Gh {
    fn run(&self, args: &[&str], stdin: Option<&str>) -> Result<Vec<u8>>;
}

pub struct SystemGh {
    path: PathBuf,
    timeout: Duration,
}

/// Network calls; generous, but a hung gh must not hold the session lock
/// forever.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(120);

impl SystemGh {
    pub fn new(path: Option<&Path>) -> Result<Self> {
        let path = path.filter(|p| p.is_file()).ok_or_else(|| {
            Error::new(ErrorCode::EGhMissing, "gh is not installed or not on PATH")
                .hint("install the GitHub CLI or set REVIEWKIT_GH")
        })?;
        let timeout = env_non_empty("REVIEWKIT_GH_TIMEOUT_MS")
            .and_then(|v| v.to_str()?.parse::<u64>().ok())
            .map(|ms| Duration::from_millis(ms.max(100)))
            .unwrap_or(DEFAULT_TIMEOUT);
        Ok(Self {
            path: path.to_path_buf(),
            timeout,
        })
    }
}

impl Gh for SystemGh {
    fn run(&self, args: &[&str], stdin: Option<&str>) -> Result<Vec<u8>> {
        let what = args.iter().take(2).copied().collect::<Vec<_>>().join(" ");
        log::debug!("gh {what}");
        let mut child = Command::new(&self.path)
            .args(args)
            .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|err| Error::new(ErrorCode::EGhMissing, format!("cannot run gh: {err}")))?;
        if let (Some(body), Some(mut pipe)) = (stdin, child.stdin.take()) {
            let body = body.to_string();
            std::thread::spawn(move || {
                let _ = pipe.write_all(body.as_bytes());
            });
        }
        let stdout = drain(child.stdout.take());
        let stderr = drain(child.stderr.take());
        let deadline = Instant::now() + self.timeout;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
                Ok(None) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(Error::new(
                        ErrorCode::EGh,
                        format!("gh {what} did not finish within {:?}", self.timeout),
                    )
                    .retryable());
                }
                Err(err) => return Err(Error::new(ErrorCode::EGh, format!("gh {what}: {err}"))),
            }
        };
        let stdout = stdout.join().unwrap_or_default();
        let stderr = stderr.join().unwrap_or_default();
        if !status.success() {
            let text = String::from_utf8_lossy(&stderr).trim().to_string();
            // gh's own wording is the only signal that a token is missing.
            if text.contains("gh auth login") || text.contains("not logged into") {
                return Err(
                    Error::new(ErrorCode::EGhAuth, format!("gh is not logged in: {text}")).hint("run `gh auth login`")
                );
            }
            // GraphQL errors arrive as a JSON body with exit 1; they describe
            // the request (not found, bad query), not the connection.
            if let Ok(body) = serde_json::from_slice::<serde_json::Value>(&stdout)
                && let Some(message) = body["errors"][0]["message"].as_str()
            {
                return Err(Error::new(ErrorCode::EGh, format!("GraphQL: {message}")));
            }
            let detail = if text.is_empty() {
                String::from_utf8_lossy(&stdout).trim().to_string()
            } else {
                text
            };
            let err = Error::new(ErrorCode::EGh, format!("gh {what} failed: {detail}"));
            return Err(if looks_transient(&detail) { err.retryable() } else { err });
        }
        Ok(stdout)
    }
}

/// Only connection-level failures are worth an automatic retry; a 404, a
/// GraphQL error or a bad request will fail the same way again. Status
/// codes are matched as tokens so a PR number like 1502 is not mistaken
/// for a 502.
fn looks_transient(detail: &str) -> bool {
    let d = detail.to_ascii_lowercase();
    if d.starts_with("graphql:") || d.contains("http 4") {
        return false;
    }
    let words: Vec<&str> = d.split(|c: char| !c.is_ascii_alphanumeric()).collect();
    let has_status = |code: &str| {
        words
            .windows(2)
            .any(|w| (w[0] == "http" || w[0] == "status") && w[1] == code)
    };
    [
        "timeout",
        "timed out",
        "dial tcp",
        "connection refused",
        "connection reset",
        "network",
        "temporarily",
    ]
    .iter()
    .any(|needle| d.contains(needle))
        || has_status("502")
        || has_status("503")
        || has_status("504")
}

fn drain(pipe: Option<impl Read + Send + 'static>) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut pipe) = pipe {
            let _ = pipe.read_to_end(&mut buf);
        }
        buf
    })
}

/// Where a pull request lives. `host` goes to `--hostname` / `--repo` so
/// GitHub Enterprise works without separate code paths.
#[derive(Debug, Clone)]
pub struct RepoRef {
    pub host: String,
    pub owner: String,
    pub name: String,
}

impl RepoRef {
    fn repo_flag(&self) -> String {
        if self.host == "github.com" {
            format!("{}/{}", self.owner, self.name)
        } else {
            format!("{}/{}/{}", self.host, self.owner, self.name)
        }
    }
}

#[derive(Debug, Clone)]
pub struct PrMeta {
    pub number: u64,
    pub url: String,
    pub title: String,
    pub author: String,
    pub state: PrState,
    pub head_ref: String,
    pub base_ref: String,
    pub base_oid: String,
    pub head_oid: String,
    pub node_id: String,
}

#[derive(Deserialize)]
struct PrView {
    number: u64,
    url: String,
    title: String,
    author: Option<Author>,
    state: String,
    #[serde(rename = "headRefName")]
    head_ref_name: String,
    #[serde(rename = "baseRefName")]
    base_ref_name: String,
    #[serde(rename = "baseRefOid")]
    base_ref_oid: String,
    #[serde(rename = "headRefOid")]
    head_ref_oid: String,
    id: String,
}

#[derive(Deserialize)]
struct Author {
    login: Option<String>,
}

pub fn pr_meta(gh: &dyn Gh, repo: &RepoRef, number: u64) -> Result<PrMeta> {
    let repo_flag = repo.repo_flag();
    let number_arg = number.to_string();
    let out = gh.run(
        &[
            "pr",
            "view",
            &number_arg,
            "--repo",
            &repo_flag,
            "--json",
            "number,url,title,author,state,headRefName,baseRefName,baseRefOid,headRefOid,id",
        ],
        None,
    )?;
    let view: PrView = serde_json::from_slice(&out)
        .map_err(|err| Error::new(ErrorCode::EGh, format!("unexpected output from gh pr view: {err}")))?;
    let state = match view.state.as_str() {
        "OPEN" => PrState::Open,
        "CLOSED" => PrState::Closed,
        "MERGED" => PrState::Merged,
        other => {
            return Err(Error::new(
                ErrorCode::EGh,
                format!("unknown pull request state {other:?}"),
            ));
        }
    };
    Ok(PrMeta {
        number: view.number,
        url: view.url,
        title: view.title,
        author: view.author.and_then(|a| a.login).unwrap_or_else(|| "ghost".into()),
        state,
        head_ref: view.head_ref_name,
        base_ref: view.base_ref_name,
        base_oid: view.base_ref_oid,
        head_oid: view.head_ref_oid,
        node_id: view.id,
    })
}

/// One query for everything the session keeps about a pull request;
/// threads page at 100, the rest is read from the first page.
const QUERY: &str = r#"query($owner: String!, $name: String!, $number: Int!, $cursor: String) {
  repository(owner: $owner, name: $name) {
    pullRequest(number: $number) {
      id title body createdAt updatedAt url
      author { login }
      reviewThreads(first: 100, after: $cursor) {
        pageInfo { hasNextPage endCursor }
        nodes {
          id path line startLine originalLine diffSide startDiffSide
          isResolved isOutdated subjectType
          comments(first: 100) { nodes { id body createdAt url author { login } } }
        }
      }
      reviews(first: 100) { nodes { state body submittedAt url author { login } } }
      comments(first: 100) { nodes { body createdAt url author { login } } }
      commits(last: 1) {
        nodes { commit { oid statusCheckRollup { state contexts(first: 100) { nodes {
          __typename
          ... on CheckRun { name status conclusion detailsUrl }
          ... on StatusContext { context state targetUrl }
        } } } } }
      }
    }
  }
}"#;

pub struct PrDetails {
    pub info: PrInfo,
    pub threads: Vec<Thread>,
}

pub fn pr_details(gh: &dyn Gh, repo: &RepoRef, number: u64, fetched_at: String) -> Result<PrDetails> {
    let mut cursor: Option<String> = None;
    let mut threads = Vec::new();
    let mut info: Option<PrInfo> = None;
    loop {
        let body = serde_json::json!({
            "query": QUERY,
            "variables": { "owner": repo.owner, "name": repo.name, "number": number, "cursor": cursor },
        })
        .to_string();
        let mut args = vec!["api", "graphql", "--input", "-"];
        if repo.host != "github.com" {
            args.extend(["--hostname", repo.host.as_str()]);
        }
        let out = gh.run(&args, Some(&body))?;
        let parsed: serde_json::Value = serde_json::from_slice(&out)
            .map_err(|err| Error::new(ErrorCode::EGh, format!("unexpected output from gh api graphql: {err}")))?;
        if let Some(message) = parsed["errors"][0]["message"].as_str() {
            return Err(Error::new(ErrorCode::EGh, format!("GraphQL: {message}")));
        }
        let pr = &parsed["data"]["repository"]["pullRequest"];
        if pr.is_null() {
            return Err(Error::new(ErrorCode::EGh, format!("pull request #{number} not found")));
        }
        if info.is_none() {
            info = Some(pr_info(pr, fetched_at.clone()));
        }
        for node in pr["reviewThreads"]["nodes"].as_array().into_iter().flatten() {
            threads.push(thread(node));
        }
        let page = &pr["reviewThreads"]["pageInfo"];
        if page["hasNextPage"].as_bool() == Some(true) {
            cursor = page["endCursor"].as_str().map(str::to_string);
            if cursor.is_none() {
                break;
            }
        } else {
            break;
        }
    }
    Ok(PrDetails {
        info: info.expect("set on the first page"),
        threads,
    })
}

fn login(node: &serde_json::Value) -> String {
    node["author"]["login"].as_str().unwrap_or("ghost").to_string()
}

fn string(node: &serde_json::Value) -> String {
    node.as_str().unwrap_or("").to_string()
}

fn side(node: &serde_json::Value) -> Option<Side> {
    match node.as_str() {
        Some("LEFT") => Some(Side::Base),
        Some("RIGHT") => Some(Side::Head),
        _ => None,
    }
}

fn thread(node: &serde_json::Value) -> Thread {
    Thread {
        id: string(&node["id"]),
        path: string(&node["path"]),
        subject_type: if node["subjectType"].as_str() == Some("FILE") {
            SubjectType::File
        } else {
            SubjectType::Line
        },
        side: side(&node["diffSide"]).unwrap_or(Side::Head),
        start_side: side(&node["startDiffSide"]),
        line: node["line"].as_u64().map(|n| n as u32),
        start_line: node["startLine"].as_u64().map(|n| n as u32),
        original_line: node["originalLine"].as_u64().map(|n| n as u32),
        resolved: node["isResolved"].as_bool().unwrap_or(false),
        outdated: node["isOutdated"].as_bool().unwrap_or(false),
        comments: node["comments"]["nodes"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|c| ThreadComment {
                id: string(&c["id"]),
                author: login(c),
                body: string(&c["body"]),
                created_at: string(&c["createdAt"]),
                url: string(&c["url"]),
            })
            .collect(),
    }
}

fn pr_info(pr: &serde_json::Value, fetched_at: String) -> PrInfo {
    let mut timeline = Vec::new();
    for r in pr["reviews"]["nodes"].as_array().into_iter().flatten() {
        timeline.push(TimelineItem {
            kind: TimelineKind::Review,
            author: login(r),
            body: r["body"].as_str().filter(|b| !b.is_empty()).map(str::to_string),
            state: r["state"].as_str().map(str::to_string),
            created_at: string(&r["submittedAt"]),
            url: string(&r["url"]),
        });
    }
    for c in pr["comments"]["nodes"].as_array().into_iter().flatten() {
        timeline.push(TimelineItem {
            kind: TimelineKind::Comment,
            author: login(c),
            body: c["body"].as_str().map(str::to_string),
            state: None,
            created_at: string(&c["createdAt"]),
            url: string(&c["url"]),
        });
    }
    timeline.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    let mut checks = Vec::new();
    let rollup = &pr["commits"]["nodes"][0]["commit"]["statusCheckRollup"];
    for c in rollup["contexts"]["nodes"].as_array().into_iter().flatten() {
        if c["__typename"].as_str() == Some("CheckRun") {
            checks.push(Check {
                name: string(&c["name"]),
                status: string(&c["status"]),
                conclusion: c["conclusion"].as_str().map(str::to_string),
                url: c["detailsUrl"].as_str().map(str::to_string),
            });
        } else {
            checks.push(Check {
                name: string(&c["context"]),
                status: string(&c["state"]),
                conclusion: None,
                url: c["targetUrl"].as_str().map(str::to_string),
            });
        }
    }
    PrInfo {
        title: string(&pr["title"]),
        body: string(&pr["body"]),
        author: login(pr),
        url: string(&pr["url"]),
        created_at: string(&pr["createdAt"]),
        updated_at: string(&pr["updatedAt"]),
        timeline,
        checks,
        fetched_at,
    }
}

/// `7`, `#7`, or a pull request URL. A URL also fixes the repository, which
/// is how a PR of a repository other than the current checkout's origin is
/// opened.
pub fn parse_pr_arg(arg: &str) -> Result<(u64, Option<RepoRef>)> {
    if let Ok(n) = arg.trim_start_matches('#').parse::<u64>() {
        return Ok((n, None));
    }
    let rest = arg
        .strip_prefix("https://")
        .or_else(|| arg.strip_prefix("http://"))
        .ok_or_else(|| Error::invalid(format!("not a pull request number or URL: {arg}")))?;
    let mut parts = rest.trim_end_matches('/').split('/');
    let host = parts.next().unwrap_or("");
    let host = host.split('@').next_back().unwrap_or(host);
    let owner = parts.next();
    let name = parts.next();
    let pull = parts.next();
    let number = parts.next().and_then(|n| n.parse::<u64>().ok());
    match (owner, name, pull, number) {
        (Some(owner), Some(name), Some("pull"), Some(number)) => Ok((
            number,
            Some(RepoRef {
                host: host.to_string(),
                owner: owner.to_string(),
                name: name.to_string(),
            }),
        )),
        _ => Err(Error::invalid(format!("not a pull request URL: {arg}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transient_detection_ignores_numbers_and_names_that_merely_contain_codes() {
        assert!(looks_transient("dial tcp: i/o timeout"));
        assert!(looks_transient("HTTP 503: Service Unavailable"));
        assert!(!looks_transient(
            "GraphQL: Could not resolve to a PullRequest with the number of 1502."
        ));
        assert!(!looks_transient(
            "HTTP 404: Not Found (https://api.github.com/repos/o/connection-pool)"
        ));
    }

    #[test]
    fn pr_args_accept_numbers_and_urls() {
        assert!(matches!(parse_pr_arg("7"), Ok((7, None))));
        assert!(matches!(parse_pr_arg("#12"), Ok((12, None))));
        let (n, repo) = parse_pr_arg("https://github.com/o/r/pull/34/files").unwrap();
        let repo = repo.unwrap();
        assert_eq!(
            (n, repo.host.as_str(), repo.owner.as_str(), repo.name.as_str()),
            (34, "github.com", "o", "r")
        );
        let (_, repo) = parse_pr_arg("https://me@ghe.example.com/o/r/pull/1").unwrap();
        assert_eq!(repo.unwrap().host, "ghe.example.com");
        assert!(parse_pr_arg("https://github.com/o/r/issues/1").is_err());
        assert!(parse_pr_arg("abc").is_err());
    }

    #[test]
    fn threads_and_info_are_read_from_graphql_nodes() {
        let pr = serde_json::json!({
            "id": "PR_1", "title": "T", "body": "B", "createdAt": "2026-01-01T00:00:00Z", "updatedAt": "2026-01-02T00:00:00Z",
            "url": "https://github.com/o/r/pull/7", "author": { "login": "alice" },
            "reviews": { "nodes": [ { "state": "APPROVED", "body": "", "submittedAt": "2026-01-03T00:00:00Z", "url": "u", "author": { "login": "bob" } } ] },
            "comments": { "nodes": [ { "body": "hi", "createdAt": "2026-01-02T12:00:00Z", "url": "c", "author": null } ] },
            "commits": { "nodes": [ { "commit": { "oid": "x", "statusCheckRollup": { "state": "FAILURE", "contexts": { "nodes": [
                { "__typename": "CheckRun", "name": "ci", "status": "COMPLETED", "conclusion": "FAILURE", "detailsUrl": "d" },
                { "__typename": "StatusContext", "context": "lint", "state": "SUCCESS", "targetUrl": null }
            ] } } } } ] }
        });
        let info = pr_info(&pr, "now".into());
        assert_eq!(info.timeline.len(), 2);
        assert_eq!(info.timeline[0].kind, TimelineKind::Comment);
        assert_eq!(info.timeline[0].author, "ghost");
        assert_eq!(info.timeline[1].state.as_deref(), Some("APPROVED"));
        assert_eq!(info.timeline[1].body, None, "empty review bodies are null");
        assert_eq!(info.checks[0].conclusion.as_deref(), Some("FAILURE"));
        assert_eq!(info.checks[1].url, None);

        let t = thread(&serde_json::json!({
            "id": "PRRT_1", "path": "src/a.rs", "line": null, "startLine": null, "originalLine": 4,
            "diffSide": "LEFT", "startDiffSide": null, "isResolved": false, "isOutdated": true, "subjectType": "LINE",
            "comments": { "nodes": [ { "id": "PRRC_1", "body": "why", "createdAt": "t", "url": "u", "author": { "login": "bob" } } ] }
        }));
        assert_eq!(t.side, Side::Base);
        assert_eq!(t.line, None);
        assert!(t.outdated);
        assert_eq!(t.comments[0].author, "bob");
    }
}
