use clap::Subcommand;

use super::stdin_json;
use crate::context::Context;
use crate::core::time;
use crate::envelope::Response;
use crate::error::Result;
use crate::model::{Decision, DecisionInput};
use crate::store::Store;

#[derive(Debug, Subcommand)]
pub enum DecisionCommand {
    /// Set the review decision. Reads a Decision from stdin.
    Set {
        #[arg(long, value_name = "ID")]
        session: String,
    },
    /// Clear the review decision.
    Clear {
        #[arg(long, value_name = "ID")]
        session: String,
    },
}

pub fn run(ctx: &Context, command: DecisionCommand) -> Result<Response> {
    let store = Store::new(ctx.state_dir.clone());
    let (session, ()) = match command {
        DecisionCommand::Set { session } => {
            let input: DecisionInput = stdin_json()?;
            store.update(&session, |s| {
                s.decision = Some(Decision {
                    event: input.event,
                    body: input.body,
                    updated_at: time::now(),
                    deliveries: vec![],
                });
                Ok(())
            })?
        }
        DecisionCommand::Clear { session } => store.update(&session, |s| {
            s.decision = None;
            Ok(())
        })?,
    };
    Response::with_session(serde_json::Value::Null, &session)
}
