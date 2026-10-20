pub mod comment;
pub mod decision;
pub mod file;
pub mod health;
pub mod history;
pub mod pr;
pub mod schema;
pub mod session;
pub mod sink;
pub mod submit;
pub mod target;
pub mod thread;
pub mod version;

use std::io::Read;

use crate::cli::Command;
use crate::context::Context;
use crate::envelope::Response;
use crate::error::{Error, Result};

pub fn dispatch(ctx: &Context, command: Command) -> Result<Response> {
    match command {
        Command::Version => version::run(ctx),
        Command::Health => health::run(ctx),
        Command::Schema => schema::run(ctx),
        Command::Session(command) => session::run(ctx, command),
        Command::Target(command) => target::run(ctx, command),
        Command::File(command) => file::run(ctx, command),
        Command::Comment(command) => comment::run(ctx, command),
        Command::Decision(command) => decision::run(ctx, command),
        Command::Pr(command) => pr::run(ctx, command),
        Command::Sink(command) => sink::run(ctx, command),
        Command::Submit(args) => submit::run(ctx, args),
        Command::Thread(command) => thread::run(ctx, command),
        Command::History(command) => history::run(ctx, command),
    }
}

/// Bodies travel on stdin so shells never have to quote them. Empty stdin
/// is rejected as invalid input rather than read as an empty document.
pub fn stdin_json<T: serde::de::DeserializeOwned>() -> Result<T> {
    crate::model::strict(stdin_value()?, "input")
}

/// For a report of something already done (a client sink's delivery):
/// refusing it would leave the delivery unrecorded, so extra keys are only
/// logged. Version skew is stopped earlier, by the contract revision check.
pub fn stdin_json_lenient<T: serde::de::DeserializeOwned>() -> Result<T> {
    let mut unknown = Vec::new();
    let parsed: std::result::Result<T, _> =
        serde_ignored::deserialize(stdin_value()?, |path| unknown.push(path.to_string()));
    let parsed = parsed.map_err(|err| Error::invalid(format!("invalid input: {err}")))?;
    if !unknown.is_empty() {
        log::warn!("ignored unknown fields: {}", unknown.join(", "));
    }
    Ok(parsed)
}

fn stdin_value() -> Result<serde_json::Value> {
    let raw = read_stdin()?;
    if raw.trim().is_empty() {
        return Err(
            Error::invalid("expected a JSON object on stdin").hint("pipe the body: echo '{...}' | reviewkit ...")
        );
    }
    serde_json::from_str(&raw).map_err(|err| Error::invalid(format!("invalid JSON on stdin: {err}")))
}

/// A terminal stdin would block forever; only a pipe or file is read.
fn read_stdin() -> Result<String> {
    use std::io::IsTerminal;
    let stdin = std::io::stdin();
    if stdin.is_terminal() {
        return Ok(String::new());
    }
    let mut raw = String::new();
    stdin.lock().read_to_string(&mut raw)?;
    Ok(raw)
}
