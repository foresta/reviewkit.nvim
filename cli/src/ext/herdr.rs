use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::error::{Error, ErrorCode, Result};
use crate::store::env_non_empty;

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
            Error::new(ErrorCode::EHerdrMissing, "herdr is not installed or not on PATH")
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
                Error::new(ErrorCode::EHerdrMissing, format!("cannot run herdr: {err}"))
                    .hint("check that REVIEWKIT_HERDR points at an executable")
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
                    return Err(Error::new(
                        ErrorCode::EHerdr,
                        format!("herdr {what} did not finish within {:?}", self.timeout),
                    )
                    .retryable());
                }
                Err(err) => return Err(Error::new(ErrorCode::EHerdr, format!("herdr {what}: {err}"))),
            }
        };
        let stdout = stdout.join().unwrap_or_default();
        let stderr = stderr.join().unwrap_or_default();
        if !status.success() {
            let stderr = String::from_utf8_lossy(&stderr).trim().to_string();
            let detail = if stderr.is_empty() {
                String::from_utf8_lossy(&stdout).trim().to_string()
            } else {
                stderr
            };
            return Err(Error::new(ErrorCode::EHerdr, format!("herdr {what} failed: {detail}")).retryable());
        }
        Ok(stdout)
    }
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
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
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
    #[serde(default)]
    pub workspace_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
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
    #[serde(default)]
    pub workspace_id: Option<String>,
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
        Error::new(
            ErrorCode::EHerdr,
            format!("unexpected output from herdr agent list: {err}"),
        )
    })?;
    Ok(parsed.result.agents)
}

pub fn panes(herdr: &dyn Herdr) -> Result<Vec<Pane>> {
    let out = herdr.run(&["pane", "list"])?;
    let parsed: Envelope<Panes> = serde_json::from_slice(&out).map_err(|err| {
        Error::new(
            ErrorCode::EHerdr,
            format!("unexpected output from herdr pane list: {err}"),
        )
    })?;
    Ok(parsed.result.panes)
}

/// Submits the prompt (text + Enter). Text goes as one argv element, so
/// multi-line prompts need no quoting.
pub fn prompt(herdr: &dyn Herdr, target: &str, text: &str) -> Result<()> {
    herdr.run(&["agent", "prompt", target, text]).map(|_| ())
}

/// Places the text in the pane's input without submitting it.
pub fn send_text(herdr: &dyn Herdr, pane: &str, text: &str) -> Result<()> {
    herdr.run(&["pane", "send-text", pane, text]).map(|_| ())
}
