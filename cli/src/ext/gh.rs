use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::Deserialize;

use crate::env::env_non_empty;
use crate::error::{Error, ErrorCode, Result};
use crate::model::session::Side;
use crate::model::{Check, PrInfo, PrState, SubjectType, Thread, ThreadComment, TimelineItem, TimelineKind};

/// The seam for tests: source-pr and sink-github talk to this trait, the
/// binary talks to `SystemGh`.
pub trait Gh {
    /// Exit status and both streams: GraphQL answers partial failures with
    /// exit 1 and a body that still carries the successful parts.
    fn run_raw(&self, args: &[&str], stdin: Option<&str>) -> Result<RawOutput>;

    /// Where gh lives, for git to call it as a credential helper.
    fn executable(&self) -> Option<&Path> {
        None
    }

    fn run(&self, args: &[&str], stdin: Option<&str>) -> Result<Vec<u8>> {
        let what = args.iter().take(2).copied().collect::<Vec<_>>().join(" ");
        let out = self.run_raw(args, stdin)?;
        if out.success {
            return Ok(out.stdout);
        }
        Err(classify_failure(&what, &out))
    }
}

pub struct RawOutput {
    pub success: bool,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// gh's own wording is the only signal that a token is missing; GraphQL
/// errors arrive as a JSON body with exit 1 and describe the request, not
/// the connection.
pub fn classify_failure(what: &str, out: &RawOutput) -> Error {
    let text = String::from_utf8_lossy(&out.stderr).trim().to_string();
    // `gh api` reports a rejected token only as the HTTP status.
    if text.contains("gh auth login")
        || text.contains("not logged into")
        || text.contains("HTTP 401")
        || text.contains("Bad credentials")
    {
        return Error::tool("gh", ErrorCode::EAuth, format!("gh is not logged in: {text}")).hint("run `gh auth login`");
    }
    if let Ok(body) = serde_json::from_slice::<serde_json::Value>(&out.stdout)
        && let Some(message) = body["errors"][0]["message"].as_str()
    {
        return graphql_error(message);
    }
    let detail = if text.is_empty() {
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    } else {
        text
    };
    let err = Error::tool("gh", ErrorCode::ETool, format!("gh {what} failed: {detail}"));
    if looks_transient(&detail) { err.retryable() } else { err }
}

/// A review left pending in the web UI blocks `addPullRequestReview`; it
/// gets its own code so the client can say what to do about it.
pub fn graphql_error(message: &str) -> Error {
    if message.to_ascii_lowercase().contains("pending review") {
        return Error::tool("gh", ErrorCode::EPendingReview, format!("GraphQL: {message}"))
            .hint("submit or discard the pending review on GitHub first");
    }
    Error::tool("gh", ErrorCode::ETool, format!("GraphQL: {message}"))
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
            Error::tool("gh", ErrorCode::EToolMissing, "gh is not installed or not on PATH")
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

/// Every gh process starts here. Forced colour or a forced TTY makes gh
/// colour its output even into a pipe, and a forced TTY can make it prompt.
pub fn command(path: &Path) -> Command {
    let mut command = Command::new(path);
    command
        .env_remove("CLICOLOR_FORCE")
        .env_remove("GH_FORCE_TTY")
        .env("NO_COLOR", "1")
        .env("GH_PROMPT_DISABLED", "1");
    command
}

impl Gh for SystemGh {
    fn executable(&self) -> Option<&Path> {
        Some(&self.path)
    }

    fn run_raw(&self, args: &[&str], stdin: Option<&str>) -> Result<RawOutput> {
        let what = args.iter().take(2).copied().collect::<Vec<_>>().join(" ");
        log::debug!("gh {what}");
        let mut child = command(&self.path)
            .args(args)
            .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|err| crate::ext::tool::spawn_error("gh", err))?;
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
                    return Err(Error::tool(
                        "gh",
                        ErrorCode::ETool,
                        format!("gh {what} did not finish within {:?}", self.timeout),
                    )
                    .retryable());
                }
                Err(err) => return Err(Error::tool("gh", ErrorCode::ETool, format!("gh {what}: {err}"))),
            }
        };
        Ok(RawOutput {
            success: status.success(),
            stdout: stdout.join().unwrap_or_default(),
            stderr: stderr.join().unwrap_or_default(),
        })
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

/// Where a pull request lives. `host` goes to `--hostname` / `--repo` on
/// every call, github.com included: without it gh follows `GH_HOST`, and a
/// GitHub Enterprise user who exports it would read and post elsewhere.
#[derive(Debug, Clone)]
pub struct RepoRef {
    pub host: String,
    pub owner: String,
    pub name: String,
}

impl RepoRef {
    fn repo_flag(&self) -> String {
        format!("{}/{}/{}", self.host, self.owner, self.name)
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
    let out = gh
        .run(
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
        )
        .map_err(|err| for_host(err, &repo.host))?;
    let view: PrView = serde_json::from_slice(&out).map_err(|err| {
        Error::tool(
            "gh",
            ErrorCode::ETool,
            format!("unexpected output from gh pr view: {err}"),
        )
    })?;
    let state = match view.state.as_str() {
        "OPEN" => PrState::Open,
        "CLOSED" => PrState::Closed,
        "MERGED" => PrState::Merged,
        other => {
            return Err(Error::tool(
                "gh",
                ErrorCode::ETool,
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
/// threads page at 100. Reviews and thread comments take the newest 100,
/// which is what reconciling an interrupted submit has to see.
const QUERY: &str = r#"query($owner: String!, $name: String!, $number: Int!, $cursor: String) {
  viewer { login }
  repository(owner: $owner, name: $name) {
    pullRequest(number: $number) {
      id title body createdAt updatedAt url
      author { login }
      reviewThreads(first: 100, after: $cursor) {
        pageInfo { hasNextPage endCursor }
        nodes {
          id path line startLine originalLine diffSide startDiffSide
          isResolved isOutdated subjectType
          root: comments(first: 1) { nodes { id body createdAt url author { login } } }
          comments(last: 100) { nodes { id body createdAt url author { login } } }
        }
      }
      reviews(last: 100) { nodes { state body submittedAt url author { login } } }
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

pub struct GraphqlResponse {
    pub data: serde_json::Value,
    pub errors: Vec<serde_json::Value>,
    /// GraphQL leaves the key out only when the request never reached
    /// execution; `data: null` may follow a mutation that already ran.
    pub has_data: bool,
}

impl GraphqlResponse {
    /// Rate limiting is checked before anything runs, and a parse or
    /// validation error (located in the document, no `data`) means the
    /// document never ran. A timeout names no location and is not this.
    /// Only when no field has data and no error names a field: one aliased
    /// field of a mutation can succeed while a later one is rate limited.
    pub fn refused_before_running(&self) -> bool {
        let nothing_ran = self
            .data
            .as_object()
            .is_none_or(|fields| fields.values().all(|v| v.is_null()));
        let unplaced = !self.errors.is_empty() && self.errors.iter().all(|e| e["path"].is_null());
        if !nothing_ran || !unplaced {
            return false;
        }
        let rate_limited = self.errors.iter().any(|e| e["type"] == "RATE_LIMITED");
        let invalid = !self.has_data
            && self
                .errors
                .iter()
                .all(|e| e["locations"].as_array().is_some_and(|l| !l.is_empty()));
        rate_limited || invalid
    }
}

/// Runs one GraphQL document and returns data and errors together, because
/// a mutation with several aliased fields can succeed in part.
pub fn graphql(gh: &dyn Gh, host: &str, query: &str, variables: serde_json::Value) -> Result<GraphqlResponse> {
    let body = serde_json::json!({ "query": query, "variables": variables }).to_string();
    let args = ["api", "graphql", "--input", "-", "--hostname", host];
    let out = gh.run_raw(&args, Some(&body))?;
    match serde_json::from_slice::<serde_json::Value>(&out.stdout) {
        Ok(mut body) if body.get("data").is_some() || body.get("errors").is_some() => Ok(GraphqlResponse {
            has_data: body.get("data").is_some(),
            data: body["data"].take(),
            errors: body["errors"].as_array().cloned().unwrap_or_default(),
        }),
        _ if out.success => Err(Error::tool(
            "gh",
            ErrorCode::ETool,
            "unexpected output from gh api graphql",
        )),
        _ => Err(for_host(classify_failure("api graphql", &out), host)),
    }
}

/// gh logs in per host; a bare `gh auth login` would log into github.com.
fn for_host(err: Error, host: &str) -> Error {
    if err.code == ErrorCode::EAuth {
        err.hint(format!("run `gh auth login --hostname {host}`"))
    } else {
        err
    }
}

pub struct PrDetails {
    pub info: PrInfo,
    pub threads: Vec<Thread>,
    /// The authenticated user, so a reconcile only matches what this user
    /// posted.
    pub viewer: String,
    /// Some optional field could not be read; fine for showing the PR, not
    /// for deciding what was already posted.
    pub incomplete: bool,
}

/// An error without a path is the whole request; under the pull request
/// itself only its threads are needed to review.
fn required_path(error: &serde_json::Value) -> bool {
    let Some(path) = error["path"].as_array() else {
        return true;
    };
    let names: Vec<&str> = path.iter().filter_map(|p| p.as_str()).collect();
    match names.as_slice() {
        ["repository", "pullRequest", field, ..] => matches!(*field, "id" | "reviewThreads"),
        ["viewer", ..] => false,
        _ => true,
    }
}

pub fn pr_details(gh: &dyn Gh, repo: &RepoRef, number: u64, fetched_at: String) -> Result<PrDetails> {
    let mut cursor: Option<String> = None;
    let mut threads = Vec::new();
    let mut info: Option<PrInfo> = None;
    let mut viewer = String::new();
    let mut incomplete = false;
    loop {
        let variables =
            serde_json::json!({ "owner": repo.owner, "name": repo.name, "number": number, "cursor": cursor });
        let response = graphql(gh, &repo.host, QUERY, variables)?;
        // Checks, reviews and the conversation are shown, not needed: a token
        // that may not read one of them must still be able to open the PR.
        let (fatal, optional): (Vec<_>, Vec<_>) = response.errors.iter().partition(|e| required_path(e));
        if let Some(message) = fatal.first().and_then(|e| e["message"].as_str()) {
            return Err(graphql_error(message));
        }
        for e in &optional {
            log::warn!("gh: ignoring an error in an optional field: {e}");
            // A token that may not read `viewer` (a GitHub App) leaves it
            // empty, which reconciling already handles on its own terms.
            if e["path"][0].as_str() != Some("viewer") {
                incomplete = true;
            }
        }
        if let Some(login) = response.data["viewer"]["login"].as_str() {
            viewer = login.to_string();
        }
        let pr = &response.data["repository"]["pullRequest"];
        if pr.is_null() {
            if let Some(message) = optional.first().and_then(|e| e["message"].as_str()) {
                return Err(graphql_error(message));
            }
            return Err(Error::tool(
                "gh",
                ErrorCode::ETool,
                format!("pull request #{number} not found"),
            ));
        }
        // A nulled field bubbles up to its nearest nullable parent while the
        // error points at the leaf, so missing threads are checked directly.
        if !pr["reviewThreads"].is_object() {
            let message = optional
                .first()
                .and_then(|e| e["message"].as_str())
                .unwrap_or("no review threads");
            return Err(graphql_error(message));
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
        viewer,
        incomplete,
    })
}

const MARKER_PREFIX: &str = "<!-- reviewkit:";
const MARKER_SUFFIX: &str = " -->";

/// The hidden line a posted body ends with, so an interrupted submit can
/// tell its own post from the same text typed on GitHub. The key is hashed
/// so the line does not expose local ids or times.
pub fn marker(key: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(key.as_bytes());
    let hex: String = digest[..6].iter().map(|b| format!("{b:02x}")).collect();
    format!("{MARKER_PREFIX}{hex}{MARKER_SUFFIX}")
}

pub fn with_marker(body: &str, marker: &str) -> String {
    format!("{}\n\n{marker}", body.trim_end())
}

pub fn has_marker(body: &str, marker: &str) -> bool {
    body.lines().any(|l| l.trim() == marker)
}

fn is_marker(line: &str) -> bool {
    let Some(hex) = line
        .trim()
        .strip_prefix(MARKER_PREFIX)
        .and_then(|rest| rest.strip_suffix(MARKER_SUFFIX))
    else {
        return false;
    };
    hex.len() == 12 && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Wherever the line stands, since `has_marker` finds it anywhere too: text
/// added after it on GitHub would otherwise show it. Only the exact shape
/// `marker` makes is ours.
pub fn strip_markers(body: &str) -> String {
    let lines: Vec<&str> = body.split_inclusive('\n').collect();
    let fenced = fenced_lines(&lines);
    let ours = |i: usize, l: &str| !fenced[i] && is_marker(l);
    if !lines.iter().enumerate().any(|(i, l)| ours(i, l)) {
        return body.to_string();
    }
    let kept: String = lines
        .iter()
        .enumerate()
        .filter(|(i, l)| !ours(*i, l))
        .map(|(_, l)| *l)
        .collect();
    kept.trim_end().to_string()
}

/// A code block shows a marker as text (a body explaining reviewkit, say).
/// A fence left open is not counted: our line is appended after the body,
/// so a body ending in an open fence would otherwise keep it forever.
fn fenced_lines(lines: &[&str]) -> Vec<bool> {
    let mut fenced = vec![false; lines.len()];
    let mut open: Option<(usize, char, usize)> = None;
    for (i, line) in lines.iter().enumerate() {
        let fence = fence_of(line);
        match (open, fence) {
            (None, Some((ch, len, _))) => open = Some((i, ch, len)),
            (Some((start, ch, len)), Some((c, l, bare))) if c == ch && l >= len && bare => {
                fenced[start..=i].iter_mut().for_each(|f| *f = true);
                open = None;
            }
            _ => {}
        }
    }
    fenced
}

/// The fence character, its run length, and whether nothing follows it.
fn fence_of(line: &str) -> Option<(char, usize, bool)> {
    let line = line.trim_end_matches(['\n', '\r']);
    let indent = line.len() - line.trim_start_matches(' ').len();
    if indent > 3 {
        return None;
    }
    let rest = &line[indent..];
    let ch = rest.chars().next().filter(|c| *c == '`' || *c == '~')?;
    let len = rest.chars().take_while(|c| *c == ch).count();
    if len < 3 {
        return None;
    }
    let after = &rest[len..];
    if ch == '`' && after.contains('`') {
        return None;
    }
    Some((ch, len, after.trim().is_empty()))
}

impl PrDetails {
    pub fn without_markers(mut self) -> Self {
        for c in self.threads.iter_mut().flat_map(|t| t.comments.iter_mut()) {
            c.body = strip_markers(&c.body);
        }
        for item in &mut self.info.timeline {
            if let Some(body) = item.body.take() {
                let stripped = strip_markers(&body);
                // A review whose body was only the marker reads as having none.
                item.body = if stripped.is_empty() && stripped != body {
                    None
                } else {
                    Some(stripped)
                };
            }
        }
        self
    }
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
        // `last: 100` alone loses the opening comment of a long thread, and
        // with it what was asked.
        comments: node["root"]["nodes"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|root| {
                !node["comments"]["nodes"]
                    .as_array()
                    .is_some_and(|last| last.iter().any(|c| c["id"] == root["id"]))
            })
            .chain(node["comments"]["nodes"].as_array().into_iter().flatten())
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
    let rest = rest.split(['?', '#']).next().unwrap_or(rest);
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

    fn response(data: serde_json::Value, errors: serde_json::Value) -> GraphqlResponse {
        GraphqlResponse {
            has_data: !data.is_null(),
            data,
            errors: errors.as_array().cloned().unwrap_or_default(),
        }
    }

    #[test]
    fn a_rate_limit_after_one_field_succeeded_is_not_refused_before_running() {
        let partial = response(
            serde_json::json!({ "review": { "id": "R1" }, "reply0": null }),
            serde_json::json!([{ "type": "RATE_LIMITED", "path": ["reply0"], "message": "slow down" }]),
        );
        assert!(!partial.refused_before_running());
        let whole = response(
            serde_json::Value::Null,
            serde_json::json!([{ "type": "RATE_LIMITED", "message": "slow down" }]),
        );
        assert!(whole.refused_before_running());
    }

    #[test]
    fn a_rejected_token_is_an_auth_error_with_the_host_to_log_into() {
        let out = RawOutput {
            success: false,
            stdout: Vec::new(),
            stderr: b"HTTP 401: Bad credentials (https://ghe.example.com/api/graphql)".to_vec(),
        };
        let err = for_host(classify_failure("api graphql", &out), "ghe.example.com");
        assert_eq!(err.code, ErrorCode::EAuth);
        assert!(err.hint.unwrap().contains("--hostname ghe.example.com"));
    }

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
    fn markers_are_found_and_stripped_whatever_the_line_endings() {
        let m = marker("k");
        assert_ne!(m, marker("k2"));
        let posted = with_marker("text\n", &m);
        assert_eq!(posted, format!("text\n\n{m}"));
        let edited = format!("edited\r\n\r\n{m}\r\n");
        assert!(has_marker(&edited, &m));
        assert!(
            !has_marker(&format!("> {m}"), &m),
            "a quoted marker is not the post's own"
        );
        assert_eq!(strip_markers(&edited), "edited");
        let appended = format!("posted\n\n{m}\nadded on GitHub");
        assert!(has_marker(&appended, &m));
        assert_eq!(strip_markers(&appended), "posted\n\nadded on GitHub");
        let lookalike = "look:\n<!-- reviewkit:not-ours -->\n> <!-- reviewkit:0123456789ab -->";
        assert_eq!(strip_markers(lookalike), lookalike, "only our exact line is stripped");
        assert_eq!(
            strip_markers("plain\r\n"),
            "plain\r\n",
            "unmarked bodies are left alone"
        );
        assert_eq!(strip_markers(&m), "");
    }

    #[test]
    fn a_marker_line_inside_a_code_block_is_text_not_a_marker() {
        let m = marker("k");
        let shown = format!("see:\n```\n{m}\n```\n~~~~\n```\n{m}\n~~~~");
        assert_eq!(strip_markers(&shown), shown);
        let posted = with_marker(&shown, &m);
        assert_eq!(strip_markers(&posted), shown, "the post's own line still goes");
        let unclosed = with_marker("```\ncode", &m);
        assert_eq!(
            strip_markers(&unclosed),
            "```\ncode",
            "an unclosed fence does not hide our line"
        );
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
        for url in [
            "https://github.com/o/r/pull/9?notification_referrer_id=x",
            "https://github.com/o/r/pull/9#discussion_r1",
            "https://github.com/o/r/pull/9/files?w=1#diff-abc",
        ] {
            assert!(matches!(parse_pr_arg(url), Ok((9, Some(_)))), "{url}");
        }
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

    #[test]
    fn a_long_thread_keeps_its_opening_comment_once() {
        let comment = |id: &str| serde_json::json!({ "id": id, "body": id, "createdAt": "t", "url": "u", "author": { "login": "a" } });
        let node = |root: &str, last: &[&str]| {
            serde_json::json!({
                "id": "PRRT_1", "path": "a", "line": 1, "diffSide": "RIGHT", "subjectType": "LINE",
                "root": { "nodes": [ comment(root) ] },
                "comments": { "nodes": last.iter().map(|id| comment(id)).collect::<Vec<_>>() }
            })
        };
        let long = thread(&node("c1", &["c50", "c51"]));
        let ids: Vec<&str> = long.comments.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, ["c1", "c50", "c51"]);
        let short = thread(&node("c1", &["c1", "c2"]));
        let ids: Vec<&str> = short.comments.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, ["c1", "c2"]);
    }
}
