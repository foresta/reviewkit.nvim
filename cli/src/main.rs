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
    let result = ctx.and_then(|ctx| reviewkit::commands::dispatch(&ctx, cli.command));
    envelope::emit(result)
}

/// clap exits with status 2 on usage errors; the envelope reserves 2 for
/// validation, so usage errors are re-routed through `Error::usage` (exit 1).
/// `--help` and `--version` keep clap's plain-text output and exit 0.
fn handle_parse_error(err: clap::Error) -> ExitCode {
    use clap::error::ErrorKind;
    match err.kind() {
        ErrorKind::DisplayHelp | ErrorKind::DisplayVersion => {
            // Unwrap is fine: printing help to a closed stdout is not recoverable.
            err.print().expect("print clap help");
            ExitCode::SUCCESS
        }
        _ => {
            let full = err.to_string();
            let first = full.lines().next().unwrap_or("invalid arguments");
            let message = first.strip_prefix("error: ").unwrap_or(first).to_string();
            envelope::emit(Err::<Response, _>(
                Error::usage(message).details(serde_json::json!({ "usage": full })),
            ))
        }
    }
}
