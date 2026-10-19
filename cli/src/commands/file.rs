use clap::Subcommand;
use serde::Serialize;

use crate::context::Context;
use crate::core::{TOO_LARGE_BYTES, files};
use crate::envelope::Response;
use crate::error::Result;
use crate::ext::git::SystemGit;
use crate::source::local::is_binary;
use crate::store::Store;

#[derive(Debug, Subcommand)]
pub enum FileCommand {
    /// Print the base-side contents of a path, as lines.
    Base {
        #[arg(long, value_name = "ID")]
        session: String,
        #[arg(long, value_name = "PATH")]
        path: String,
    },
}

/// `lines` rather than raw bytes so the client can hand it straight to a
/// buffer, and so binary content never reaches stdout.
#[derive(Serialize)]
struct BaseFile {
    exists: bool,
    binary: bool,
    too_large: bool,
    lines: Option<Vec<String>>,
}

pub fn run(ctx: &Context, command: FileCommand) -> Result<Response> {
    let store = Store::new(ctx.state_dir.clone());
    match command {
        FileCommand::Base { session, path } => {
            let session = store.load(&session)?;
            files::check_path(&path)?;
            let git = SystemGit::new(ctx.tools.git.as_deref())?;
            let repo = files::object_repo(&git, &session);
            let rev = &session.base.sha;
            let path = files::base_git_path(&session, &path);
            if !repo.is_blob(rev, path) {
                return Response::new(BaseFile {
                    exists: false,
                    binary: false,
                    too_large: false,
                    lines: None,
                });
            }
            let bytes = repo.read_blob(rev, path)?;
            let binary = is_binary(&bytes);
            let too_large = bytes.len() > TOO_LARGE_BYTES;
            let lines = (!binary && !too_large).then(|| files::split_lines(&String::from_utf8_lossy(&bytes)));
            Response::new(BaseFile {
                exists: true,
                binary,
                too_large,
                lines,
            })
        }
    }
}
