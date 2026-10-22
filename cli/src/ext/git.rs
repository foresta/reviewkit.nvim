use std::ffi::OsString;
use std::io::Read;
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::env::env_non_empty;
use crate::error::{Error, ErrorCode, Result};

/// Commands that talk to a remote. Only these get a deadline: local commands
/// on a large repository can legitimately take long, a remote that stopped
/// answering never finishes.
const NETWORK_COMMANDS: &[&str] = &["fetch", "clone"];

fn staging_dir(mirror: &Path) -> PathBuf {
    let mut name = mirror.file_name().map(OsString::from).unwrap_or_default();
    name.push(format!(".tmp-{}", std::process::id()));
    mirror.with_file_name(name)
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

/// The seam for tests: core logic talks to this trait, the binary talks to
/// `SystemGit`.
pub trait Git {
    fn run(&self, cwd: &Path, args: &[&str]) -> Result<Vec<u8>>;
}

pub struct SystemGit {
    path: PathBuf,
    network_timeout: Duration,
    partial_clones: std::sync::Mutex<std::collections::HashMap<PathBuf, bool>>,
}

impl SystemGit {
    pub fn new(path: Option<&Path>) -> Result<Self> {
        let path = path.ok_or_else(|| {
            Error::tool("git", ErrorCode::EToolMissing, "git is not installed or not on PATH")
                .hint("install git or set REVIEWKIT_GIT")
        })?;
        let network_timeout = env_non_empty("REVIEWKIT_GIT_TIMEOUT_MS")
            .and_then(|v| v.to_str().and_then(|s| s.parse::<u64>().ok()))
            .filter(|ms| *ms > 0)
            .map(Duration::from_millis)
            .unwrap_or(Duration::from_secs(120));
        Ok(Self {
            path: path.to_path_buf(),
            network_timeout,
            partial_clones: Default::default(),
        })
    }
}

/// `GIT_CONFIG_PARAMETERS` / `GIT_CONFIG_COUNT` stay: they are the user's
/// own `git -c` settings, and the ones that change output are pinned per
/// command anyway.
const INHERITED_REPOSITORY_ENV: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_COMMON_DIR",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_NAMESPACE",
    "GIT_PREFIX",
    "GIT_QUARANTINE_PATH",
    "GIT_LITERAL_PATHSPECS",
    "GIT_GLOB_PATHSPECS",
    "GIT_NOGLOB_PATHSPECS",
    "GIT_ICASE_PATHSPECS",
];

impl Git for SystemGit {
    fn run(&self, cwd: &Path, args: &[&str]) -> Result<Vec<u8>> {
        // Spawning in a missing directory fails like a missing program would.
        if !cwd.is_dir() {
            return Err(checkout_not_found(cwd));
        }
        let bounded = subcommand(args).is_some_and(|a| NETWORK_COMMANDS.contains(&a)) || self.is_partial_clone(cwd);
        self.exec(cwd, args, bounded)
    }
}

impl SystemGit {
    /// Any command in a partial clone may fetch a missing blob, so all of
    /// them get the network deadline there.
    fn is_partial_clone(&self, cwd: &Path) -> bool {
        let mut known = self.partial_clones.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(partial) = known.get(cwd) {
            return *partial;
        }
        let partial = has_promisor(|args| self.exec(cwd, args, false));
        known.insert(cwd.to_path_buf(), partial);
        partial
    }

    fn exec(&self, cwd: &Path, args: &[&str], bounded: bool) -> Result<Vec<u8>> {
        log::debug!("git {}", args.join(" "));
        let mut command = Command::new(&self.path);
        command
            // Paths with non-ASCII bytes would otherwise come back quoted and
            // escaped in diff headers.
            .args(["-c", "core.quotepath=false"])
            // An editor's or desktop's askpass would open a dialog and wait
            // with a lock held, like the terminal prompts turned off below.
            .args(["-c", "core.askPass="])
            .args(args)
            .current_dir(cwd)
            .env("LC_ALL", "C")
            // Would silently override -U3.
            .env_remove("GIT_DIFF_OPTS")
            // There is no terminal to answer a credential prompt; waiting for
            // one would hang while holding a lock.
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GCM_INTERACTIVE", "never")
            .env_remove("GIT_ASKPASS")
            .env_remove("SSH_ASKPASS")
            // OpenSSH before 8.4 ignores SSH_ASKPASS_REQUIRE and starts its
            // default askpass whenever there is a display and no terminal.
            .env_remove("DISPLAY")
            .env("SSH_ASKPASS_REQUIRE", "never")
            // The watcher refreshes often; a status that takes index.lock
            // makes the user's own `git add` fail at random.
            .env("GIT_OPTIONAL_LOCKS", "0")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // Set by a hook or a commit editor that started Neovim; they would
        // point every command (and the mirror's `remote set-url`) at another
        // repository, or change how the paths given are matched.
        for var in INHERITED_REPOSITORY_ENV {
            command.env_remove(var);
        }
        // ssh asks for host keys and passphrases on /dev/tty, over the
        // editor's screen and while a lock is held; without a controlling
        // terminal it fails instead. The user's ssh command is kept.
        #[cfg(unix)]
        unsafe {
            use std::os::unix::process::CommandExt;
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = command.spawn().map_err(|err| {
            if cwd.is_dir() {
                crate::ext::tool::spawn_error("git", err)
            } else {
                checkout_not_found(cwd)
            }
        })?;
        let stdout = drain(child.stdout.take());
        let stderr = drain(child.stderr.take());
        let deadline = bounded.then(|| Instant::now() + self.network_timeout);
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if deadline.is_none_or(|d| Instant::now() < d) => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Ok(None) => {
                    // The whole process group: ssh and credential helpers would
                    // otherwise outlive the git that was given up on.
                    #[cfg(unix)]
                    unsafe {
                        libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL);
                    }
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(Error::tool(
                        "git",
                        ErrorCode::EGit,
                        format!(
                            "git {} did not finish within {:?}",
                            subcommand(args).unwrap_or("command"),
                            self.network_timeout
                        ),
                    )
                    .hint("set REVIEWKIT_GIT_TIMEOUT_MS to wait longer (it also bounds every command in a partial clone, which may fetch missing blobs)")
                    .retryable());
                }
                Err(err) => {
                    return Err(Error::tool(
                        "git",
                        ErrorCode::EGit,
                        format!("git {}: {err}", args.join(" ")),
                    ));
                }
            }
        };
        let stdout = stdout.join().unwrap_or_default();
        if !status.success() {
            let stderr = String::from_utf8_lossy(&stderr.join().unwrap_or_default())
                .trim()
                .to_string();
            let code = failure_code(&stderr);
            let mut err = Error::tool("git", code, format!("git {} failed: {stderr}", args.join(" ")))
                .details(serde_json::json!({ "exit": status.code() }));
            if code == ErrorCode::EAuth {
                err =
                    err.hint("check the credentials for this remote; for GitHub run `gh auth login --hostname <host>`");
            }
            return Err(err);
        }
        Ok(stdout)
    }
}

fn checkout_not_found(cwd: &Path) -> Error {
    Error::tool("git", ErrorCode::EGit, format!("checkout not found: {}", cwd.display()))
        .hint("it was moved or removed; open the review again from where the checkout is now")
}

/// What makes git fetch missing objects on demand: either setting alone is
/// enough.
fn has_promisor(run: impl Fn(&[&str]) -> Result<Vec<u8>>) -> bool {
    run(&["config", "--get", "extensions.partialclone"]).is_ok()
        || run(&["config", "--type=bool", "--get-regexp", r"^remote\..*\.promisor$"])
            .is_ok_and(|out| text(&out).lines().any(|l| l.ends_with(" true")))
}

fn subcommand<'a>(args: &[&'a str]) -> Option<&'a str> {
    let mut rest = args;
    while let ["-c", _, tail @ ..] = rest {
        rest = tail;
    }
    rest.first().copied()
}

/// `-c` values that make gh answer git's credential requests for one host.
/// The empty value first drops the helpers configured for that host, so a
/// token gh hands out is not also stored into the user's keychain.
pub fn gh_credential_config(host: &str, gh: &Path) -> Vec<String> {
    let quoted = format!("'{}'", gh.to_string_lossy().replace('\'', "'\\''"));
    vec![
        format!("credential.https://{host}.helper="),
        format!("credential.https://{host}.helper=!{quoted} auth git-credential"),
    ]
}

/// GitHub answers an unauthenticated fetch of a private repository with
/// "not found", so that counts as a credentials problem too.
fn failure_code(stderr: &str) -> ErrorCode {
    let auth = [
        "Authentication failed",
        "could not read Username",
        "could not read Password",
        "Repository not found",
    ];
    if auth.iter().any(|needle| stderr.contains(needle)) {
        ErrorCode::EAuth
    } else {
        ErrorCode::EGit
    }
}

/// Path and status come from `--raw` rather than the patch text, whose
/// headers are shaped by user config and quoting rules.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawChange {
    pub status: char,
    pub old_path: Option<String>,
    pub path: String,
    pub base_blob: Option<String>,
    pub head_blob: Option<String>,
}

/// All paths handed in and out are relative to the repository root.
pub struct Repository<'a> {
    git: &'a dyn Git,
    root: PathBuf,
}

impl<'a> Repository<'a> {
    pub fn discover(git: &'a dyn Git, cwd: &Path) -> Result<Self> {
        let out = git.run(cwd, &["rev-parse", "--show-toplevel"]).map_err(|err| {
            if err.code == ErrorCode::EToolMissing {
                return err;
            }
            Error::tool(
                "git",
                ErrorCode::EGit,
                format!("not inside a git repository: {}", cwd.display()),
            )
            .details(serde_json::json!({ "cause": err.message }))
        })?;
        let root = PathBuf::from(text(&out).trim_end());
        Ok(Self { git, root })
    }

    pub fn git(&self) -> &'a dyn Git {
        self.git
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

    /// Read from config rather than `remote get-url`, which applies
    /// `url.<x>.insteadOf` rewrites: a proxy or test rewrite must change
    /// where fetches go, not which repository this is.
    pub fn remote_url(&self, name: &str) -> Option<String> {
        self.run(&["config", "--get", &format!("remote.{name}.url")])
            .ok()
            .map(|out| text(&out).trim_end().to_string())
            .filter(|url| !url.is_empty())
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
            // The user's diff.renameLimit would turn renames into a delete
            // plus an add, here and not on GitHub. Git's own default rather
            // than -l0, which means 32767: rename detection is quadratic and
            // runs under the session lock.
            "-l1000",
            "--abbrev=40",
            "--no-color",
            "--no-ext-diff",
            // The same files diff_patch lists: a submodule with only local
            // edits would otherwise be a Target with no patch.
            "--no-textconv",
            "--ignore-submodules=dirty",
        ];
        args.extend(Self::range_args(base, head));
        parse_raw(&self.run(&args)?)
    }

    /// Prefixes, blank-line form, inter-hunk context, algorithm and textconv
    /// are all pinned because the user's diff config would otherwise change
    /// the text the parser sees (the algorithm moves hunk bounds away from
    /// GitHub's, and textconv would describe a file the editor never shows).
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
            "-l1000",
            "--no-color",
            "--no-ext-diff",
            "--src-prefix=a/",
            "--dst-prefix=b/",
            "--inter-hunk-context=0",
            "--diff-algorithm=myers",
            "--indent-heuristic",
            "--no-textconv",
            "--ignore-submodules=dirty",
            "--submodule=short",
        ];
        args.extend(Self::range_args(base, head));
        Ok(text(&self.run(&args)?))
    }

    pub fn diff_blobs(&self, old: &str, new: &str) -> Result<Vec<crate::model::Hunk>> {
        let out = self.run(&[
            "diff",
            "-U0",
            "--no-color",
            "--no-ext-diff",
            "--diff-algorithm=myers",
            "--indent-heuristic",
            "--no-textconv",
            "--src-prefix=a/",
            "--dst-prefix=b/",
            old,
            new,
        ])?;
        Ok(crate::ext::diffparse::parse(&text(&out))
            .into_iter()
            .next()
            .map(|patch| patch.hunks)
            .unwrap_or_default())
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

    /// Untracked files do not count.
    pub fn is_dirty(&self) -> Result<bool> {
        let out = self.run(&["status", "--porcelain", "--untracked-files=no"])?;
        Ok(!out.is_empty())
    }

    /// One git per batch rather than per file: a refresh of a working tree
    /// with hundreds of changed or untracked files would otherwise spawn
    /// hundreds of processes. A batch that fails (a file vanished
    /// meanwhile) is retried per path so one file does not blank the rest.
    pub fn hash_objects(&self, paths: &[String]) -> Vec<Option<String>> {
        let mut ids = Vec::with_capacity(paths.len());
        for chunk in paths.chunks(200) {
            let mut args = vec!["hash-object", "--"];
            args.extend(chunk.iter().map(String::as_str));
            match self.run(&args) {
                Ok(out) => {
                    let lines: Vec<String> = text(&out).lines().map(str::to_string).collect();
                    if lines.len() == chunk.len() {
                        ids.extend(lines.into_iter().map(Some));
                        continue;
                    }
                    ids.extend(chunk.iter().map(|p| self.hash_object(p).ok()));
                }
                Err(_) => ids.extend(chunk.iter().map(|p| self.hash_object(p).ok())),
            }
        }
        ids
    }

    pub fn hash_object(&self, path: &str) -> Result<String> {
        let out = self.run(&["hash-object", "--", path])?;
        Ok(text(&out).trim_end().to_string())
    }

    /// `-w` stores the blob: a working-tree file has no commit to pin, and
    /// following a comment later needs the old content to diff against. Only
    /// commented files are stored; the objects dangle and `git gc` collects
    /// them like any stash leftover.
    ///
    /// Writing an object git already has still touches its mtime, which
    /// file watchers see as a change in `.git/objects`; existing objects are
    /// therefore only looked up.
    pub fn hash_object_write(&self, path: &str) -> Result<String> {
        let id = self.hash_object(path)?;
        if self.run(&["cat-file", "-e", &id]).is_ok() {
            return Ok(id);
        }
        let out = self.run(&["hash-object", "-w", "--", path])?;
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

    /// `ls-tree` rather than `cat-file -s` per file: one git per batch.
    /// `--literal-pathspecs` because a path such as `:x` is pathspec magic
    /// otherwise; a batch that still fails is sized per file, since a size
    /// not found reads as small.
    pub fn blob_sizes(&self, rev: &str, paths: &[String]) -> std::collections::HashMap<String, u64> {
        let mut sizes = std::collections::HashMap::new();
        for chunk in paths.chunks(200) {
            let mut args = vec![
                "--literal-pathspecs",
                "ls-tree",
                "-r",
                "-l",
                "-z",
                "--full-tree",
                rev,
                "--",
            ];
            args.extend(chunk.iter().map(String::as_str));
            let Ok(out) = self.run(&args) else {
                for path in chunk {
                    if let Some(size) = self.blob_size(rev, path) {
                        sizes.insert(path.clone(), size);
                    }
                }
                continue;
            };
            for entry in text(&out).split('\0') {
                let Some((meta, path)) = entry.split_once('\t') else {
                    continue;
                };
                if let Some(size) = meta.split_whitespace().nth(3).and_then(|s| s.parse().ok()) {
                    sizes.insert(path.to_string(), size);
                }
            }
        }
        sizes
    }

    pub fn blob_size(&self, rev: &str, path: &str) -> Option<u64> {
        self.run(&["cat-file", "-s", &format!("{rev}:{path}")])
            .ok()
            .and_then(|out| text(&out).trim_end().parse().ok())
    }

    pub fn is_blob(&self, rev: &str, path: &str) -> bool {
        self.run(&["cat-file", "-t", &format!("{rev}:{path}")])
            .map(|out| text(&out).trim_end() == "blob")
            .unwrap_or(false)
    }

    /// The `shallow` file rather than `--is-shallow-repository`, which git
    /// before 2.15 does not know and would answer as "not shallow".
    pub fn is_shallow(&self) -> bool {
        self.run(&["rev-parse", "--git-path", "shallow"])
            .is_ok_and(|out| self.root.join(text(&out).trim_end()).exists())
    }

    /// A partial clone lacks blobs it fetches on demand from its promisor;
    /// a bare clone of it would miss them with no remote to ask.
    pub fn is_partial(&self) -> bool {
        has_promisor(|args| self.run(args))
    }

    pub fn has_commit(&self, sha: &str) -> bool {
        self.run(&["cat-file", "-e", &format!("{sha}^{{commit}}")]).is_ok()
    }

    pub fn merge_base(&self, a: &str, b: &str) -> Result<String> {
        let out = self.run(&["merge-base", a, b])?;
        Ok(text(&out).trim_end().to_string())
    }

    pub fn rev_parse(&self, rev: &str) -> Option<String> {
        self.run(&["rev-parse", "--verify", "--quiet", rev])
            .ok()
            .map(|out| text(&out).trim_end().to_string())
    }

    /// A bare clone from the user's checkout: no network, and objects are
    /// hard-linked so the mirror starts cheap. The user's repository is
    /// only read.
    pub fn clone_bare(git: &'a dyn Git, source_root: &Path, mirror: &Path, remote_url: &str) -> Result<Self> {
        let parent = mirror
            .parent()
            .ok_or_else(|| Error::internal("mirror path has no parent"))?;
        std::fs::create_dir_all(parent)?;
        // Built beside the final path and renamed into place, so a failure or
        // a crash never leaves a half-made mirror that looks usable.
        let staging = staging_dir(mirror);
        let _ = std::fs::remove_dir_all(&staging);
        let built = git
            .run(
                parent,
                &[
                    "clone",
                    "--bare",
                    "--quiet",
                    &source_root.to_string_lossy(),
                    &staging.to_string_lossy(),
                ],
            )
            .and_then(|_| Self::at(git, staging.clone()).run(&["remote", "set-url", "origin", remote_url]))
            .and_then(|_| std::fs::rename(&staging, mirror).map_err(Error::from));
        if let Err(err) = built {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(err);
        }
        Ok(Self::at(git, mirror.to_path_buf()))
    }

    pub fn init_bare(git: &'a dyn Git, mirror: &Path, remote_url: &str) -> Result<Self> {
        let staging = staging_dir(mirror);
        let _ = std::fs::remove_dir_all(&staging);
        std::fs::create_dir_all(&staging)?;
        let built = git
            .run(&staging, &["init", "--quiet", "--bare"])
            .and_then(|_| Self::at(git, staging.clone()).run(&["remote", "add", "origin", remote_url]))
            .and_then(|_| std::fs::rename(&staging, mirror).map_err(Error::from));
        if let Err(err) = built {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(err);
        }
        Ok(Self::at(git, mirror.to_path_buf()))
    }

    pub fn fetch_with(&self, config: &[String], remote: &str, refspecs: &[String]) -> Result<()> {
        let mut args: Vec<&str> = config.iter().flat_map(|c| ["-c", c.as_str()]).collect();
        args.extend(["fetch", "--quiet", "--no-tags", remote]);
        args.extend(refspecs.iter().map(String::as_str));
        self.run(&args).map(|_| ())
    }

    /// The worktree is reviewkit's own, so tracked changes in it are
    /// overwritten to put it at the requested commit; untracked files are
    /// left alone. A worktree whose `.git` points at a vanished mirror is
    /// recreated rather than reported, since nothing in it is the user's.
    pub fn worktree_checkout(&self, dir: &Path, sha: &str) -> Result<()> {
        if dir.join(".git").exists()
            && self
                .git
                .run(dir, &["checkout", "--quiet", "--force", "--detach", sha])
                .is_ok()
        {
            return Ok(());
        }
        let _ = std::fs::remove_dir_all(dir);
        let _ = self.run(&["worktree", "prune"]);
        self.run(&[
            "worktree",
            "add",
            "--quiet",
            "--force",
            "--detach",
            &dir.to_string_lossy(),
            sha,
        ])
        .map(|_| ())
    }

    pub fn worktree_remove(&self, dir: &Path) {
        if self
            .run(&["worktree", "remove", "--force", &dir.to_string_lossy()])
            .is_err()
        {
            let _ = std::fs::remove_dir_all(dir);
            let _ = self.run(&["worktree", "prune"]);
        }
    }
}

/// `https://host/owner/name(.git)`, `ssh://git@host/owner/name` and
/// `git@host:owner/name(.git)`. Anything else is left unparsed rather than
/// guessed. The port is dropped: gh names a host without one.
pub fn parse_remote_url(url: &str) -> Option<(Option<String>, Option<String>, Option<String>)> {
    let (host, path) = split_remote_url(url)?;
    let path = path.trim_end_matches('/').trim_end_matches(".git");
    let (owner, name) = path.rsplit_once('/')?;
    Some((Some(host.to_string()), Some(owner.to_string()), Some(name.to_string())))
}

fn split_remote_url(url: &str) -> Option<(&str, &str)> {
    fn without_user(host: &str) -> &str {
        host.rsplit('@').next().unwrap_or(host)
    }
    fn without_port(host: &str) -> &str {
        host.split(':').next().unwrap_or(host)
    }
    if let Some(rest) = url.strip_prefix("https://").or_else(|| url.strip_prefix("http://")) {
        let (host, path) = rest.split_once('/')?;
        Some((without_port(without_user(host)), path))
    } else if let Some(rest) = url.strip_prefix("ssh://") {
        let (host, path) = rest.split_once('/')?;
        Some((without_port(without_user(host)), path))
    } else {
        let (_, rest) = url.split_once('@')?;
        let (host, path) = rest.split_once(':')?;
        Some((host, path))
    }
}

/// `:<old mode> <new mode> <old sha> <new sha> <status>[score]\0<path>\0`
/// with a second path for R and C.
fn parse_raw(bytes: &[u8]) -> Result<Vec<RawChange>> {
    let mut fields = bytes.split(|b| *b == 0);
    let mut changes = Vec::new();
    while let Some(meta) = fields.next() {
        if meta.is_empty() {
            continue;
        }
        let meta = String::from_utf8_lossy(meta);
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
        // Paths travel as JSON strings; a lossy name would match no file.
        let (Some(path), Ok(old_path)) = (utf8_path(path), old_path.map(|p| utf8_path(p).ok_or(())).transpose()) else {
            log::warn!(
                "skipping a change whose path is not UTF-8: {}",
                String::from_utf8_lossy(path)
            );
            continue;
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

fn utf8_path(bytes: &[u8]) -> Option<String> {
    std::str::from_utf8(bytes).ok().map(str::to_string)
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

    /// Answers a batched hash-object with one line fewer than asked, and a
    /// single-path one with that path's id.
    struct ShortBatch;

    impl Git for ShortBatch {
        fn run(&self, _cwd: &Path, args: &[&str]) -> Result<Vec<u8>> {
            let paths = &args[2..];
            if paths.len() > 1 {
                return Ok(paths[1..]
                    .iter()
                    .map(|p| format!("id-{p}\n"))
                    .collect::<String>()
                    .into_bytes());
            }
            if paths[0] == "gone" {
                return Err(Error::tool("git", ErrorCode::EGit, "no such file"));
            }
            Ok(format!("id-{}\n", paths[0]).into_bytes())
        }
    }

    struct FailingBatch;

    impl Git for FailingBatch {
        fn run(&self, _cwd: &Path, args: &[&str]) -> Result<Vec<u8>> {
            let paths = &args[2..];
            if paths.len() > 1 {
                return Err(Error::tool("git", ErrorCode::EGit, "a file vanished"));
            }
            Ok(format!("id-{}\n", paths[0]).into_bytes())
        }
    }

    #[test]
    fn a_failing_batch_is_hashed_per_path() {
        let repo = Repository::at(&FailingBatch, PathBuf::from("/repo"));
        let ids = repo.hash_objects(&["a".into(), "b".into()]);
        assert_eq!(ids, vec![Some("id-a".to_string()), Some("id-b".to_string())]);
    }

    #[test]
    fn a_batch_answering_too_few_lines_is_hashed_per_path_without_shifting_ids() {
        let repo = Repository::at(&ShortBatch, PathBuf::from("/repo"));
        let ids = repo.hash_objects(&["a".into(), "gone".into(), "c".into()]);
        assert_eq!(ids, vec![Some("id-a".to_string()), None, Some("id-c".to_string())]);
    }

    #[test]
    fn config_before_the_subcommand_keeps_the_network_timeout() {
        assert_eq!(subcommand(&["fetch", "origin"]), Some("fetch"));
        assert_eq!(
            subcommand(&["-c", "a=b", "-c", "c=d", "fetch", "origin"]),
            Some("fetch")
        );
        assert_eq!(subcommand(&["-c", "a=b"]), None);
    }

    #[test]
    fn the_gh_credential_helper_is_scoped_to_one_host_and_quoted() {
        let config = gh_credential_config("ghe.example.com", Path::new("/opt/it's/gh"));
        assert_eq!(
            config,
            vec![
                "credential.https://ghe.example.com.helper=".to_string(),
                "credential.https://ghe.example.com.helper=!'/opt/it'\\''s/gh' auth git-credential".to_string(),
            ]
        );
    }

    #[test]
    fn rejected_credentials_are_auth_errors() {
        for stderr in [
            "fatal: Authentication failed for 'https://github.com/o/r.git/'",
            "fatal: could not read Username for 'https://github.com': terminal prompts disabled",
            "remote: Repository not found.\nfatal: repository 'https://github.com/o/private.git/' not found",
        ] {
            assert_eq!(failure_code(stderr), ErrorCode::EAuth, "{stderr}");
        }
        assert_eq!(failure_code("fatal: bad revision 'nope'"), ErrorCode::EGit);
    }

    #[test]
    fn remote_urls_parse_to_host_owner_name() {
        let expect = Some((
            Some("github.com".into()),
            Some("foresta".into()),
            Some("reviewkit.nvim".into()),
        ));
        assert_eq!(
            parse_remote_url("https://github.com/foresta/reviewkit.nvim.git"),
            expect
        );
        assert_eq!(parse_remote_url("git@github.com:foresta/reviewkit.nvim.git"), expect);
        assert_eq!(parse_remote_url("ssh://git@github.com/foresta/reviewkit.nvim"), expect);
        assert_eq!(parse_remote_url("/local/path"), None);
    }

    #[test]
    fn a_port_is_not_part_of_the_host() {
        let ghe = |url: &str| parse_remote_url(url).and_then(|(host, _, _)| host);
        assert_eq!(
            ghe("ssh://git@ghe.example.com:2222/o/r.git").as_deref(),
            Some("ghe.example.com")
        );
        assert_eq!(
            ghe("https://ghe.example.com:8443/o/r.git").as_deref(),
            Some("ghe.example.com")
        );
        assert_eq!(
            ghe("https://user@ghe.example.com:8443/o/r").as_deref(),
            Some("ghe.example.com")
        );
        assert_eq!(ghe("git@ghe:o/r.git").as_deref(), Some("ghe"), "a dotless host is kept");
    }

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
