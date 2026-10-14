use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::error::{Error, ErrorCode, Result};

/// The seam for tests: core logic talks to this trait, the binary talks to
/// `SystemGit`.
pub trait Git {
    fn run(&self, cwd: &Path, args: &[&str]) -> Result<Vec<u8>>;
}

pub struct SystemGit {
    path: PathBuf,
}

impl SystemGit {
    pub fn new(path: Option<&Path>) -> Result<Self> {
        let path = path.ok_or_else(|| {
            Error::new(ErrorCode::EGit, "git is not installed or not on PATH").hint("install git or set REVIEWKIT_GIT")
        })?;
        Ok(Self {
            path: path.to_path_buf(),
        })
    }
}

impl Git for SystemGit {
    fn run(&self, cwd: &Path, args: &[&str]) -> Result<Vec<u8>> {
        log::debug!("git {}", args.join(" "));
        let output = Command::new(&self.path)
            // Paths with non-ASCII bytes would otherwise come back quoted and
            // escaped in diff headers.
            .args(["-c", "core.quotepath=false"])
            .args(args)
            .current_dir(cwd)
            .env("LC_ALL", "C")
            // Would silently override -U3.
            .env_remove("GIT_DIFF_OPTS")
            .stdin(Stdio::null())
            .output()
            .map_err(|err| Error::new(ErrorCode::EGit, format!("cannot run git: {err}")))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Err(
                Error::new(ErrorCode::EGit, format!("git {} failed: {stderr}", args.join(" ")))
                    .details(serde_json::json!({ "exit": output.status.code() })),
            );
        }
        Ok(output.stdout)
    }
}

/// One entry of `git diff --raw`: the authoritative path and status of a
/// change. The patch text is only mined for hunks, because its headers are
/// shaped by user config and quoting rules.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawChange {
    pub status: char,
    pub old_path: Option<String>,
    pub path: String,
    pub base_blob: Option<String>,
    pub head_blob: Option<String>,
}

/// One repository as seen from its root. All paths handed in and out are
/// relative to that root.
pub struct Repository<'a> {
    git: &'a dyn Git,
    root: PathBuf,
}

impl<'a> Repository<'a> {
    pub fn discover(git: &'a dyn Git, cwd: &Path) -> Result<Self> {
        let out = git.run(cwd, &["rev-parse", "--show-toplevel"]).map_err(|err| {
            Error::new(
                ErrorCode::EGit,
                format!("not inside a git repository: {}", cwd.display()),
            )
            .details(serde_json::json!({ "cause": err.message }))
        })?;
        let root = PathBuf::from(text(&out).trim_end());
        Ok(Self { git, root })
    }

    pub fn at(git: &'a dyn Git, root: PathBuf) -> Self {
        Self { git, root }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn run(&self, args: &[&str]) -> Result<Vec<u8>> {
        self.git.run(&self.root, args)
    }

    /// `--quiet` makes "not a revision" exit 1, which is the user's mistake
    /// (E_INVALID); anything else is git itself failing (E_GIT).
    pub fn resolve_commit(&self, spec: &str) -> Result<String> {
        let out = self
            .run(&[
                "rev-parse",
                "--verify",
                "--quiet",
                "--end-of-options",
                &format!("{spec}^{{commit}}"),
            ])
            .map_err(|err| match err.details.get("exit").and_then(|e| e.as_i64()) {
                Some(1) => Error::invalid(format!("not a commit: {spec}"))
                    .hint("pass a branch, tag or commit as --base / --head"),
                _ => err,
            })?;
        Ok(text(&out).trim_end().to_string())
    }

    pub fn remote_url(&self, name: &str) -> Option<String> {
        self.run(&["remote", "get-url", name])
            .ok()
            .map(|out| text(&out).trim_end().to_string())
    }

    fn range_args<'b>(base: &'b str, head: Option<&'b str>) -> Vec<&'b str> {
        let mut args = vec![base];
        args.extend(head);
        args.push("--");
        args
    }

    /// `--abbrev=40` because `--full-index` widens blob ids only in patch
    /// headers, not in `--raw` lines.
    pub fn diff_raw(&self, base: &str, head: Option<&str>) -> Result<Vec<RawChange>> {
        let mut args = vec![
            "diff",
            "--raw",
            "-z",
            "-M",
            "--abbrev=40",
            "--no-color",
            "--no-ext-diff",
        ];
        args.extend(Self::range_args(base, head));
        parse_raw(&self.run(&args)?)
    }

    /// Prefixes, blank-line form, inter-hunk context and textconv are all
    /// pinned because the user's diff config would otherwise change the text
    /// the parser sees (and textconv would describe a file the editor never
    /// shows).
    pub fn diff_patch(&self, base: &str, head: Option<&str>) -> Result<String> {
        let mut args = vec![
            "-c",
            "diff.noprefix=false",
            "-c",
            "diff.mnemonicPrefix=false",
            "-c",
            "diff.suppressBlankEmpty=false",
            "diff",
            "-U3",
            "-M",
            "--no-color",
            "--no-ext-diff",
            "--src-prefix=a/",
            "--dst-prefix=b/",
            "--inter-hunk-context=0",
            "--no-textconv",
            "--ignore-submodules=dirty",
            "--submodule=short",
        ];
        args.extend(Self::range_args(base, head));
        Ok(text(&self.run(&args)?))
    }

    /// Nested repositories are listed as `dir/` and are not files to review.
    pub fn untracked(&self) -> Result<Vec<OsString>> {
        let out = self.run(&["ls-files", "--others", "--exclude-standard", "-z"])?;
        Ok(out
            .split(|b| *b == 0)
            .filter(|p| !p.is_empty() && !p.ends_with(b"/"))
            .map(|p| OsString::from_vec(p.to_vec()))
            .collect())
    }

    /// Tracked files only: whether the working tree differs from HEAD.
    pub fn is_dirty(&self) -> Result<bool> {
        let out = self.run(&["status", "--porcelain", "--untracked-files=no"])?;
        Ok(!out.is_empty())
    }

    pub fn hash_object(&self, path: &str) -> Result<String> {
        let out = self.run(&["hash-object", "--", path])?;
        Ok(text(&out).trim_end().to_string())
    }

    pub fn blob_id(&self, rev: &str, path: &str) -> Option<String> {
        self.run(&["rev-parse", "--verify", "--quiet", &format!("{rev}:{path}")])
            .ok()
            .map(|out| text(&out).trim_end().to_string())
    }

    /// `cat-file blob` rather than `show`, which would also print trees and
    /// apply textconv filters.
    pub fn read_blob(&self, rev: &str, path: &str) -> Result<Vec<u8>> {
        self.run(&["cat-file", "blob", &format!("{rev}:{path}")])
    }

    pub fn is_blob(&self, rev: &str, path: &str) -> bool {
        self.run(&["cat-file", "-t", &format!("{rev}:{path}")])
            .map(|out| text(&out).trim_end() == "blob")
            .unwrap_or(false)
    }
}

/// `:<old mode> <new mode> <old sha> <new sha> <status>[score]\0<path>\0`
/// with a second path for R and C.
fn parse_raw(bytes: &[u8]) -> Result<Vec<RawChange>> {
    let mut fields = bytes
        .split(|b| *b == 0)
        .map(|f| String::from_utf8_lossy(f).into_owned());
    let mut changes = Vec::new();
    while let Some(meta) = fields.next() {
        if meta.is_empty() {
            continue;
        }
        let parts: Vec<&str> = meta.trim_start_matches(':').split(' ').collect();
        let [_, _, old_sha, new_sha, status, ..] = parts.as_slice() else {
            return Err(Error::internal(format!("unexpected git diff --raw entry: {meta:?}")));
        };
        let status = status.chars().next().unwrap_or('M');
        let first = fields
            .next()
            .ok_or_else(|| Error::internal("git diff --raw entry without a path"))?;
        let (old_path, path) = if matches!(status, 'R' | 'C') {
            let second = fields
                .next()
                .ok_or_else(|| Error::internal("git diff --raw rename without a target"))?;
            (Some(first), second)
        } else {
            (None, first)
        };
        changes.push(RawChange {
            status,
            old_path,
            path,
            base_blob: blob(old_sha),
            head_blob: blob(new_sha),
        });
    }
    Ok(changes)
}

fn blob(id: &str) -> Option<String> {
    Some(id.to_string()).filter(|id| !id.is_empty() && !id.bytes().all(|b| b == b'0'))
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_entries_parse_modify_rename_and_typechange() {
        let a = "a".repeat(40);
        let b = "b".repeat(40);
        let z = "0".repeat(40);
        let raw = format!(
            ":100644 100644 {a} {b} M\0sp ace.txt\0:100644 100644 {a} {a} R100\0old.txt\0new.txt\0:120000 100644 {a} {b} T\0link\0:000000 100644 {z} {b} A\0added\0"
        );
        let changes = parse_raw(raw.as_bytes()).unwrap();
        assert_eq!(changes.len(), 4);
        assert_eq!((changes[0].status, changes[0].path.as_str()), ('M', "sp ace.txt"));
        assert_eq!(changes[1].old_path.as_deref(), Some("old.txt"));
        assert_eq!(changes[1].path, "new.txt");
        assert_eq!(changes[2].status, 'T');
        assert_eq!(changes[3].base_blob, None);
        assert_eq!(changes[3].head_blob.as_deref(), Some(b.as_str()));
    }
}
