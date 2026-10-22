use std::process::ExitCode;

use clap::Parser;
use reviewkit::cli::Cli;
use reviewkit::envelope::{self, Response};
use reviewkit::{Error, logging};

fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(err) => return handle_parse_error(err),
    };

    let ctx = cli.global.resolve();
    if let Ok(ctx) = &ctx {
        logging::init(ctx);
    }
    // A panic would otherwise exit 101 with no envelope, and the client
    // could not tell a bug from a tool it failed to run.
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        ctx.and_then(|ctx| reviewkit::commands::dispatch(&ctx, cli.command))
    }))
    .unwrap_or_else(|payload| {
        let message = payload
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "unknown panic".into());
        Err(Error::internal(format!("reviewkit panicked: {message}")))
    });
    envelope::emit(result)
}

/// clap exits with status 2 on usage errors; the envelope reserves 2 for
/// validation, so usage errors are re-routed through `Error::usage` (exit 1).
/// `--help` and `--version` keep clap's plain-text output and exit 0.
fn handle_parse_error(err: clap::Error) -> ExitCode {
    use clap::error::ErrorKind;
    match err.kind() {
        ErrorKind::DisplayHelp | ErrorKind::DisplayVersion => match err.print() {
            Ok(()) => ExitCode::SUCCESS,
            Err(_) => ExitCode::from(reviewkit::ErrorCode::EIo.exit_code()),
        },
        _ => {
            let full = err.to_string();
            envelope::emit(Err::<Response, _>(
                Error::usage(usage_message(&full)).details(serde_json::json!({ "usage": full })),
            ))
        }
    }
}

/// Not clap's first line alone: clap lists what is missing on the lines
/// under it.
fn usage_message(full: &str) -> String {
    let mut lines = full.lines().take_while(|l| !l.trim().is_empty());
    let first = lines.next().unwrap_or("invalid arguments");
    let first = first.strip_prefix("error: ").unwrap_or(first);
    let rest: Vec<&str> = lines.map(str::trim).collect();
    if rest.is_empty() {
        first.to_string()
    } else {
        format!("{first} {}", rest.join(", "))
    }
}
