use clap::Subcommand;
use serde::Serialize;

use crate::context::Context;
use crate::core::files::is_binary;
use crate::core::{TOO_LARGE_BYTES, files};
use crate::envelope::Response;
use crate::error::Result;
use crate::ext::git::SystemGit;
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
    /// Base64 of the raw bytes when they are not UTF-8: only the editor
    /// knows which encoding the head side was read with, so the decoding is
    /// left to it and `lines` is a lossy fallback.
    #[serde(skip_serializing_if = "Option::is_none")]
    raw: Option<String>,
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
                    raw: None,
                });
            }
            let bytes = repo.read_blob(rev, path)?;
            let binary = is_binary(&bytes);
            let too_large = bytes.len() > TOO_LARGE_BYTES;
            let readable = !binary && !too_large;
            let lines = readable.then(|| files::split_lines(&String::from_utf8_lossy(&bytes)));
            let raw = (readable && std::str::from_utf8(&bytes).is_err()).then(|| base64(&bytes));
            Response::new(BaseFile {
                exists: true,
                binary,
                too_large,
                lines,
                raw,
            })
        }
    }
}

fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(TABLE[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn base64_matches_the_standard_alphabet_and_padding() {
        assert_eq!(super::base64(b""), "");
        assert_eq!(super::base64(b"f"), "Zg==");
        assert_eq!(super::base64(b"fo"), "Zm8=");
        assert_eq!(super::base64(b"foo"), "Zm9v");
        assert_eq!(super::base64(&[0x82, 0xa0]), "gqA=");
    }
}
