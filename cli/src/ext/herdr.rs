use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::env::env_non_empty;
use crate::error::{Error, ErrorCode, Result};

/// The seam for tests: the sink talks to this trait, the binary talks to
/// `SystemHerdr`.
pub trait Herdr {
    fn run(&self, args: &[&str]) -> Result<Vec<u8>>;
}

pub struct SystemHerdr {
    path: PathBuf,
    timeout: Duration,
}

/// herdr talks to a local server; anything slower than this is a hang, and
/// the caller holds the session lock while waiting.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

impl SystemHerdr {
    /// A configured path that does not exist is reported as missing rather
    /// than as a failed run, so the hint points at installation.
    pub fn new(path: Option<&Path>) -> Result<Self> {
        let path = path.filter(|p| p.is_file()).ok_or_else(|| {
            Error::tool(
                "herdr",
                ErrorCode::EToolMissing,
                "herdr is not installed or not on PATH",
            )
            .hint("install herdr or set REVIEWKIT_HERDR")
        })?;
        // Clamped so a stray `0` cannot make every call time out instantly.
        let timeout = env_non_empty("REVIEWKIT_HERDR_TIMEOUT_MS")
            .and_then(|v| v.to_str()?.parse::<u64>().ok())
            .map(|ms| Duration::from_millis(ms.max(100)))
            .unwrap_or(DEFAULT_TIMEOUT);
        Ok(Self {
            path: path.to_path_buf(),
            timeout,
        })
    }
}

impl Herdr for SystemHerdr {
    fn run(&self, args: &[&str]) -> Result<Vec<u8>> {
        // Only the subcommand is named in messages: the prompt text is one
        // of the args and would otherwise be copied into every last_error.
        let what = args.iter().take(2).copied().collect::<Vec<_>>().join(" ");
        log::debug!("herdr {what}");
        let mut child = Command::new(&self.path)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|err| {
                let err = crate::ext::tool::spawn_error("herdr", err);
                if err.code == ErrorCode::EToolMissing {
                    err.hint("check that REVIEWKIT_HERDR points at an executable")
                } else {
                    err
                }
            })?;
        // Pipes are drained on threads so a large `agent list` cannot fill
        // the pipe and deadlock against the wait below.
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
                        "herdr",
                        ErrorCode::ETool,
                        format!("herdr {what} did not finish within {:?}", self.timeout),
                    )
                    .retryable());
                }
                Err(err) => return Err(Error::tool("herdr", ErrorCode::ETool, format!("herdr {what}: {err}"))),
            }
        };
        let stdout = stdout.join().unwrap_or_default();
        let stderr = stderr.join().unwrap_or_default();
        if !status.success() {
            return Err(failure(&what, &stdout, &stderr));
        }
        Ok(stdout)
    }
}

#[derive(Deserialize)]
struct ErrorBody {
    error: ErrorDetail,
}

#[derive(Deserialize)]
struct ErrorDetail {
    code: String,
    #[serde(default)]
    message: String,
}

/// The target does not exist, so no text can have reached a pane.
const NOT_FOUND: &[&str] = &["agent_not_found", "pane_not_found"];

fn failure(what: &str, stdout: &[u8], stderr: &[u8]) -> Error {
    let stderr = String::from_utf8_lossy(stderr).trim().to_string();
    let detail = if stderr.is_empty() {
        String::from_utf8_lossy(stdout).trim().to_string()
    } else {
        stderr
    };
    let Ok(ErrorBody { error }) = serde_json::from_str::<ErrorBody>(&detail) else {
        return Error::tool("herdr", ErrorCode::ETool, format!("herdr {what} failed: {detail}")).retryable();
    };
    let message = if error.message.is_empty() {
        &error.code
    } else {
        &error.message
    };
    let err = Error::tool("herdr", ErrorCode::ETool, format!("herdr {what} failed: {message}"));
    if NOT_FOUND.contains(&error.code.as_str()) {
        // Not retryable: the same target is just as missing next time.
        return err
            .details(serde_json::json!({ "not_started": true, "herdr_code": error.code }))
            .hint("the agent or pane is gone; pick another target");
    }
    err.details(serde_json::json!({ "herdr_code": error.code })).retryable()
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

/// The subset of `herdr agent list` the sink needs. Extra fields are
/// ignored and `pane_id` may be absent, so one odd entry never breaks the
/// whole listing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Agent {
    #[serde(default)]
    pub pane_id: Option<String>,
    pub agent: String,
    #[serde(default)]
    pub agent_status: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub foreground_cwd: Option<String>,
    #[serde(default)]
    pub terminal_title_stripped: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pane {
    pub pane_id: String,
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub agent_status: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub foreground_cwd: Option<String>,
    #[serde(default)]
    pub terminal_title_stripped: Option<String>,
}

#[derive(Deserialize)]
struct Envelope<T> {
    result: T,
}

#[derive(Deserialize)]
struct Agents {
    agents: Vec<Agent>,
}

#[derive(Deserialize)]
struct Panes {
    panes: Vec<Pane>,
}

pub fn agents(herdr: &dyn Herdr) -> Result<Vec<Agent>> {
    let out = herdr.run(&["agent", "list"])?;
    let parsed: Envelope<Agents> = serde_json::from_slice(&out).map_err(|err| {
        Error::tool(
            "herdr",
            ErrorCode::ETool,
            format!("unexpected output from herdr agent list: {err}"),
        )
    })?;
    Ok(parsed.result.agents)
}

pub fn panes(herdr: &dyn Herdr) -> Result<Vec<Pane>> {
    let out = herdr.run(&["pane", "list"])?;
    let parsed: Envelope<Panes> = serde_json::from_slice(&out).map_err(|err| {
        Error::tool(
            "herdr",
            ErrorCode::ETool,
            format!("unexpected output from herdr pane list: {err}"),
        )
    })?;
    Ok(parsed.result.panes)
}

/// Submits the prompt (text + Enter). Text goes as one argv element, so
/// multi-line prompts need no quoting.
pub fn prompt(herdr: &dyn Herdr, target: &str, text: &str) -> Result<()> {
    herdr
        .run(&["agent", "prompt", target, text])
        .map(|_| ())
        .map_err(sent_maybe)
}

/// herdr keeps no record to reconcile with, so only the agent's screen can
/// tell whether a failed send arrived.
fn sent_maybe(err: Error) -> Error {
    if crate::ext::tool::never_started(&err) {
        return err;
    }
    err.outcome_unknown("check the agent's pane before sending again")
}

/// Places the text in the pane's input without submitting it.
pub fn send_text(herdr: &dyn Herdr, pane: &str, text: &str) -> Result<()> {
    herdr
        .run(&["pane", "send-text", pane, text])
        .map(|_| ())
        .map_err(sent_maybe)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_target_herdr_does_not_know_is_not_started_and_other_errors_are_not() {
        let missing = failure(
            "pane send-text",
            b"",
            br#"{"error":{"code":"pane_not_found","message":"pane p not found"},"id":"cli:request"}"#,
        );
        assert!(crate::ext::tool::never_started(&missing));
        assert!(!missing.retryable);
        assert_eq!(missing.message, "herdr pane send-text failed: pane p not found");
        let other = failure(
            "agent prompt",
            b"",
            br#"{"error":{"code":"server_busy","message":"busy"}}"#,
        );
        assert!(!crate::ext::tool::never_started(&other));
        assert_eq!(other.details["herdr_code"], "server_busy");
        let plain = failure("agent prompt", b"", b"pane gone");
        assert!(!crate::ext::tool::never_started(&plain));
        assert_eq!(plain.message, "herdr agent prompt failed: pane gone");
    }
}
