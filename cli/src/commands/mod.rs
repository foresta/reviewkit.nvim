pub mod file;
pub mod health;
pub mod schema;
pub mod session;
pub mod target;
pub mod version;

use crate::cli::Command;
use crate::context::Context;
use crate::envelope::Response;
use crate::error::Result;

pub fn dispatch(ctx: &Context, command: Command) -> Result<Response> {
    match command {
        Command::Version => version::run(ctx),
        Command::Health => health::run(ctx),
        Command::Schema => schema::run(ctx),
        Command::Session(command) => session::run(ctx, command),
        Command::Target(command) => target::run(ctx, command),
        Command::File(command) => file::run(ctx, command),
    }
}
