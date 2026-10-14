pub mod health;
pub mod schema;
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
    }
}
