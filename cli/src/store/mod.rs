mod state_dir;

pub use state_dir::{StateDir, env_non_empty};

use std::fs::File;
use std::io::Write;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::core::{etags, time};
use crate::error::{Error, ErrorCode, Result};
use crate::model::{SCHEMA_REVISION, SCHEMA_VERSION, Session};

pub struct Store {
    dir: StateDir,
    lock_timeout: Duration,
}

impl Store {
    pub fn new(dir: StateDir) -> Self {
        Self {
            dir,
            lock_timeout: Duration::from_secs(2),
        }
    }

    pub fn with_lock_timeout(mut self, timeout: Duration) -> Self {
        self.lock_timeout = timeout;
        self
    }

    pub fn state_dir(&self) -> &StateDir {
        &self.dir
    }

    pub fn exists(&self, id: &str) -> bool {
        self.dir.session_file(id).map(|p| p.is_file()).unwrap_or(false)
    }

    pub fn load(&self, id: &str) -> Result<Session> {
        let path = self.dir.session_file(id)?;
        read_session(&path, id).map(|(session, _)| session)
    }

    fn load_for_write(&self, id: &str) -> Result<Session> {
        let path = self.dir.session_file(id)?;
        match read_session(&path, id)? {
            (session, None) => Ok(session),
            (_, Some(writer)) => Err(newer_writer(&path, &writer)),
        }
    }

    /// First save. The lock is taken even here so two `session open` calls
    /// racing on the same id cannot both think they created it.
    pub fn create(&self, mut session: Session) -> Result<Session> {
        let dir = self.dir.session_dir(&session.id)?;
        std::fs::create_dir_all(&dir)?;
        let id = session.id.clone();
        self.with_lock(&id, || {
            if self.exists(&id) {
                return Err(Error::new(
                    ErrorCode::EConflict,
                    format!("session already exists: {id}"),
                ));
            }
            session.rev = 0;
            self.commit(&mut session)?;
            Ok(session)
        })
    }

    /// The closure sees the freshly read session, never a cached one, so
    /// concurrent writers from other processes are serialized by the lock
    /// rather than lost.
    pub fn update<T>(&self, id: &str, apply: impl FnOnce(&mut Session) -> Result<T>) -> Result<(Session, T)> {
        self.with_lock(id, || {
            let mut session = self.load_for_write(id)?;
            let value = apply(&mut session)?;
            self.commit(&mut session)?;
            Ok((session, value))
        })
    }

    /// Takes the lock first so a concurrent `update` finishes (or fails with
    /// E_LOCKED) instead of writing into a half-deleted directory.
    pub fn remove(&self, id: &str) -> Result<()> {
        let dir = self.dir.session_dir(id)?;
        if !dir.is_dir() {
            return Err(no_session(id));
        }
        self.with_lock(id, || {
            std::fs::remove_dir_all(&dir)?;
            Ok(())
        })?;
        self.remove_empty_parents(&dir);
        Ok(())
    }

    /// `session open` semantics in one lock: resume when the session exists,
    /// create it otherwise, then apply the refresh either way. A first open
    /// that fails before its first save leaves only a lock file behind;
    /// `list` would skip it, but it should not exist at all.
    pub fn open_or_create<T>(
        &self,
        id: &str,
        init: impl FnOnce() -> Result<Session>,
        apply: impl FnOnce(&mut Session) -> Result<T>,
    ) -> Result<(Session, T)> {
        let dir = self.dir.session_dir(id)?;
        let created = !dir.exists();
        std::fs::create_dir_all(&dir)?;
        let result = self.with_lock(id, || {
            let mut session = if self.exists(id) {
                self.load_for_write(id)?
            } else {
                let mut session = init()?;
                if session.id != id {
                    return Err(Error::internal(format!(
                        "init produced session {:?}, expected {id:?}",
                        session.id
                    )));
                }
                session.rev = 0;
                session
            };
            let value = apply(&mut session)?;
            self.commit(&mut session)?;
            Ok((session, value))
        });
        if result.is_err() && created && !self.exists(id) {
            let _ = std::fs::remove_dir_all(&dir);
            self.remove_empty_parents(&dir);
        }
        result
    }

    pub fn list(&self) -> Result<Vec<Session>> {
        let mut sessions = Vec::new();
        let root = self.dir.sessions_dir();
        if root.is_dir() {
            collect_sessions(&root, &mut sessions);
        }
        sessions.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        Ok(sessions)
    }

    /// Session ids nest directories (`pr/<host>/<owner>/<repo>/<n>`); the
    /// intermediate ones are only worth keeping while a session lives under them.
    fn remove_empty_parents(&self, dir: &Path) {
        let root = self.dir.sessions_dir();
        let mut current = dir.parent();
        while let Some(parent) = current {
            if parent == root || std::fs::remove_dir(parent).is_err() {
                break;
            }
            current = parent.parent();
        }
    }

    fn commit(&self, session: &mut Session) -> Result<()> {
        let path = self.dir.session_file(&session.id)?;
        session.rev += 1;
        session.schema_version = SCHEMA_VERSION;
        session.state_path = path.clone();
        session.updated_at = time::now();
        session.etags = etags::compute(session);
        let mut value = serde_json::to_value(&*session).map_err(serialize_error)?;
        if let Some(object) = value.as_object_mut() {
            object.insert(
                WRITER_KEY.into(),
                serde_json::to_value(Writer::current()).map_err(serialize_error)?,
            );
        }
        write_atomically(&path, &serde_json::to_vec_pretty(&value).map_err(serialize_error)?)
    }

    /// Runs `f` under the session's advisory lock. Polls instead of blocking
    /// so a stuck process cannot hang the editor; the lock is held only for
    /// one read-modify-write. The critical section is a closure rather than a
    /// returned guard because the borrow checker cannot yet return a guard
    /// out of a retry loop.
    fn with_lock<T>(&self, id: &str, f: impl FnOnce() -> Result<T>) -> Result<T> {
        lock_while(&self.dir.lock_file(id)?, self.lock_timeout, &format!("session {id}"), f)
    }

    /// Serializes slow work on one session (fetching a pull request) without
    /// holding the data lock: the shared PR worktree and pr.json must not be
    /// written by two fetches at once, and a fetch that started earlier must
    /// not land after a newer one. Waits as long as a fetch can take.
    pub fn with_fetch_lock<T>(&self, id: &str, f: impl FnOnce() -> Result<T>) -> Result<T> {
        if !self.dir.session_dir(id)?.is_dir() {
            return Err(no_session(id));
        }
        lock_while(
            &self.dir.fetch_lock_file(id)?,
            FETCH_LOCK_TIMEOUT,
            &format!("session {id}"),
            f,
        )
    }
}

pub const FETCH_LOCK_TIMEOUT: Duration = Duration::from_secs(180);

/// Runs `f` while holding an advisory lock on `path`. Polls instead of
/// blocking so a stuck holder turns into E_LOCKED rather than a hang.
pub fn lock_while<T>(path: &Path, timeout: Duration, what: &str, f: impl FnOnce() -> Result<T>) -> Result<T> {
    use std::io::ErrorKind::{Interrupted, WouldBlock};
    let file = File::options().create(true).truncate(false).write(true).open(path)?;
    let mut lock = fd_lock::RwLock::new(file);
    let deadline = Instant::now() + timeout;
    loop {
        match lock.try_write() {
            Ok(_guard) => return f(),
            Err(err) if matches!(err.kind(), WouldBlock | Interrupted) => {
                if Instant::now() >= deadline {
                    return Err(
                        Error::new(ErrorCode::ELocked, format!("{what} is locked by another process")).retryable(),
                    );
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(err) => return Err(err.into()),
        }
    }
}

const WRITER_KEY: &str = "_writer";

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
struct Writer {
    #[serde(default)]
    revision: u32,
    #[serde(default)]
    version: String,
    #[serde(default)]
    exe: String,
}

impl Writer {
    fn current() -> Self {
        Writer {
            revision: SCHEMA_REVISION,
            version: env!("CARGO_PKG_VERSION").into(),
            exe: std::env::current_exe()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
        }
    }
}

fn newer_writer(path: &Path, writer: &Writer) -> Error {
    Error::new(
        ErrorCode::ESchemaVersion,
        format!(
            "{} was last written by reviewkit {} ({}), which knows more than this reviewkit {}",
            path.display(),
            writer.version,
            writer.exe,
            env!("CARGO_PKG_VERSION")
        ),
    )
    .hint(format!(
        "update this reviewkit ({}) to {} or later (the plugin's :ReviewInstall), or use {}",
        std::env::current_exe()
            .map(|p| p.display().to_string())
            .unwrap_or_default(),
        writer.version,
        writer.exe
    ))
    .details(serde_json::json!({
        "direction": "newer",
        "found_revision": writer.revision,
        "expected_revision": SCHEMA_REVISION,
        "written_by": { "version": writer.version, "exe": writer.exe },
    }))
}

/// The writer is returned only when it is newer than this CLI: the session
/// may then be shown but must not be written back.
fn read_session(path: &Path, id: &str) -> Result<(Session, Option<Writer>)> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Err(no_session(id)),
        Err(err) => return Err(err.into()),
    };
    // The version is checked on the raw value first: a mismatched file would
    // otherwise surface as a confusing field-level parse error.
    let raw: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|err| corrupt(path, format!("not JSON: {err}")))?;
    let found = raw.get("schema_version").and_then(|v| v.as_u64()).unwrap_or(0);
    if found != u64::from(SCHEMA_VERSION) {
        let newer = found > u64::from(SCHEMA_VERSION);
        // Purging is only advice for a file this CLI has outgrown; for a newer
        // one it would throw away comments a newer CLI can still read.
        let hint = if newer {
            "upgrade with :ReviewInstall, or use the reviewkit that wrote it"
        } else {
            "close the session with --purge, or use the reviewkit version that wrote it"
        };
        return Err(Error::new(
            ErrorCode::ESchemaVersion,
            format!(
                "{} was written with schema_version {found}, this reviewkit speaks {SCHEMA_VERSION}",
                path.display()
            ),
        )
        .hint(hint)
        .details(serde_json::json!({
            "direction": if newer { "newer" } else { "older" },
            "found": found,
            "expected": SCHEMA_VERSION,
        })));
    }
    let writer: Writer = raw
        .get(WRITER_KEY)
        .and_then(|w| serde_json::from_value(w.clone()).ok())
        .unwrap_or_default();
    let newer = (writer.revision > SCHEMA_REVISION).then_some(writer);
    match (serde_json::from_value::<Session>(raw), newer) {
        (Ok(session), newer) => Ok((session, newer)),
        (Err(_), Some(writer)) => Err(newer_writer(path, &writer)),
        (Err(err), None) => Err(corrupt(path, err.to_string())),
    }
}

/// Never fails as a whole: one unreadable entry or session must not hide the
/// others from `list`. Symlinks are not followed, so a loop cannot recurse forever.
fn collect_sessions(dir: &Path, out: &mut Vec<Session>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) => {
            log::warn!("skipping {}: {err}", dir.display());
            return;
        }
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.is_dir() {
            collect_sessions(&path, out);
        } else if meta.is_file() && path.file_name().is_some_and(|n| n == "session.json") {
            match read_session(&path, "") {
                Ok((session, _)) => out.push(session),
                Err(err) => log::warn!("skipping {}: {err}", path.display()),
            }
        }
    }
}

fn write_atomically(path: &Path, bytes: &[u8]) -> Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| Error::internal("session file has no parent directory"))?;
    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
    tmp.write_all(bytes)?;
    tmp.as_file().sync_all()?;
    tmp.persist(path)
        .map_err(|err| Error::internal(format!("cannot replace {}: {}", path.display(), err.error)))?;
    Ok(())
}

fn no_session(id: &str) -> Error {
    Error::new(ErrorCode::ENoSession, format!("no such session: {id}")).hint("run `reviewkit session list`")
}

fn corrupt(path: &Path, detail: String) -> Error {
    Error::internal(format!("cannot read {}: {detail}", path.display()))
}

fn serialize_error(err: serde_json::Error) -> Error {
    Error::internal(format!("cannot serialize session: {err}"))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::model::{Decision, DecisionEvent, HeadRef, Repo, RevRef, Session, Source};

    fn store(dir: &tempfile::TempDir) -> Store {
        Store::new(StateDir::new(dir.path().to_path_buf())).with_lock_timeout(Duration::from_millis(100))
    }

    fn session(id: &str) -> Session {
        Session::new(
            id.to_string(),
            Repo {
                root: PathBuf::from("/repo"),
                host: None,
                owner: None,
                name: None,
                remote_url: None,
            },
            Source::Local {
                base_spec: "HEAD~1".into(),
                head_spec: "worktree".into(),
                worktree: PathBuf::from("/repo"),
            },
            RevRef {
                r#ref: Some("HEAD~1".into()),
                sha: "a".repeat(40),
            },
            HeadRef {
                r#ref: None,
                sha: None,
                dirty: true,
            },
            time::now(),
        )
    }

    #[test]
    fn create_then_load_roundtrips_and_sets_rev_and_state_path() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        let created = store.create(session("local/abc/HEAD~1..worktree")).unwrap();
        assert_eq!(created.rev, 1);
        assert!(created.state_path.ends_with("session.json"));
        let loaded = store.load("local/abc/HEAD~1..worktree").unwrap();
        assert_eq!(loaded.rev, 1);
        assert_eq!(loaded.etags, created.etags);
        assert_eq!(loaded.state_path, created.state_path);
    }

    #[test]
    fn update_bumps_rev_and_changes_only_the_touched_etag() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        let before = store.create(session("local/abc/x")).unwrap();
        let (after, ()) = store
            .update("local/abc/x", |s| {
                s.decision = Some(Decision {
                    event: DecisionEvent::Approve,
                    body: "lgtm".into(),
                    updated_at: time::now(),
                });
                Ok(())
            })
            .unwrap();
        assert_eq!(after.rev, 2);
        assert_ne!(after.etags.decision, before.etags.decision);
        assert_eq!(after.etags.targets, before.etags.targets);
        assert_eq!(after.etags.comments, before.etags.comments);
        assert_eq!(after.etags.viewed, before.etags.viewed);
    }

    #[test]
    fn etags_are_stable_across_saves_without_changes() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        let first = store.create(session("local/abc/x")).unwrap();
        let (second, ()) = store.update("local/abc/x", |_| Ok(())).unwrap();
        assert_eq!(second.rev, first.rev + 1);
        assert_eq!(second.etags, first.etags);
    }

    #[test]
    fn failed_apply_leaves_the_file_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        store.create(session("local/abc/x")).unwrap();
        let err = store
            .update::<()>("local/abc/x", |s| {
                s.rev = 99;
                Err(Error::invalid("nope"))
            })
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::EInvalid);
        assert_eq!(store.load("local/abc/x").unwrap().rev, 1);
    }

    #[test]
    fn missing_session_is_e_no_session() {
        let dir = tempfile::tempdir().unwrap();
        let err = store(&dir).load("local/none/x").unwrap_err();
        assert_eq!(err.code, ErrorCode::ENoSession);
    }

    #[test]
    fn creating_twice_is_a_conflict() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        store.create(session("local/abc/x")).unwrap();
        let err = store.create(session("local/abc/x")).unwrap_err();
        assert_eq!(err.code, ErrorCode::EConflict);
    }

    #[test]
    fn other_schema_version_is_e_schema_version() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        let created = store.create(session("local/abc/x")).unwrap();
        let mut raw: serde_json::Value = serde_json::from_slice(&std::fs::read(&created.state_path).unwrap()).unwrap();
        raw["schema_version"] = serde_json::json!(999);
        std::fs::write(&created.state_path, serde_json::to_vec(&raw).unwrap()).unwrap();
        let err = store.load("local/abc/x").unwrap_err();
        assert_eq!(err.code, ErrorCode::ESchemaVersion);
        assert_eq!(err.details["found"], 999);
    }

    fn written_by_revision(
        store: &Store,
        id: &str,
        revision: u32,
        tweak: impl FnOnce(&mut serde_json::Value),
    ) -> PathBuf {
        let created = store.create(session(id)).unwrap();
        let mut raw: serde_json::Value = serde_json::from_slice(&std::fs::read(&created.state_path).unwrap()).unwrap();
        raw["_writer"] = serde_json::json!({ "revision": revision, "version": "9.9.9", "exe": "/new/reviewkit" });
        raw["added_later"] = serde_json::json!({ "keep": true });
        tweak(&mut raw);
        std::fs::write(&created.state_path, serde_json::to_vec(&raw).unwrap()).unwrap();
        created.state_path
    }

    #[test]
    fn a_session_from_a_newer_cli_is_readable_but_never_written() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        let path = written_by_revision(&store, "local/abc/x", SCHEMA_REVISION + 1, |_| {});
        let before = std::fs::read(&path).unwrap();
        assert_eq!(store.load("local/abc/x").unwrap().id, "local/abc/x");
        assert_eq!(store.list().unwrap().len(), 1);

        let err = store.update("local/abc/x", |_| Ok(())).unwrap_err();
        assert_eq!(err.code, ErrorCode::ESchemaVersion);
        assert_eq!(err.details["direction"], "newer");
        assert_eq!(err.details["written_by"]["exe"], "/new/reviewkit");
        let err = store
            .open_or_create("local/abc/x", || unreachable!(), |_| Ok(()))
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::ESchemaVersion);
        assert_eq!(std::fs::read(&path).unwrap(), before);

        store.remove("local/abc/x").unwrap();
    }

    #[test]
    fn an_unknown_value_from_a_newer_cli_is_a_version_error_not_corruption() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        written_by_revision(&store, "local/abc/x", SCHEMA_REVISION + 1, |raw| {
            raw["source"]["kind"] = serde_json::json!("gitlab");
        });
        let err = store.load("local/abc/x").unwrap_err();
        assert_eq!(err.code, ErrorCode::ESchemaVersion);
        assert_eq!(err.details["direction"], "newer");
    }

    #[test]
    fn a_session_without_a_writer_is_updated_and_gets_one() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        let created = store.create(session("local/abc/x")).unwrap();
        let mut raw: serde_json::Value = serde_json::from_slice(&std::fs::read(&created.state_path).unwrap()).unwrap();
        raw.as_object_mut().unwrap().remove("_writer");
        std::fs::write(&created.state_path, serde_json::to_vec(&raw).unwrap()).unwrap();
        store.update("local/abc/x", |_| Ok(())).unwrap();
        let raw: serde_json::Value = serde_json::from_slice(&std::fs::read(&created.state_path).unwrap()).unwrap();
        assert_eq!(raw["_writer"]["revision"], SCHEMA_REVISION);
    }

    #[test]
    fn an_older_schema_version_suggests_purging_but_a_newer_one_does_not() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        for (version, direction, purge) in [(0, "older", true), (999, "newer", false)] {
            let created = store.create(session(&format!("local/abc/{version}"))).unwrap();
            let mut raw: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&created.state_path).unwrap()).unwrap();
            raw["schema_version"] = serde_json::json!(version);
            std::fs::write(&created.state_path, serde_json::to_vec(&raw).unwrap()).unwrap();
            let err = store.load(&created.id).unwrap_err();
            assert_eq!(err.details["direction"], direction);
            assert_eq!(err.hint.as_deref().unwrap_or("").contains("--purge"), purge);
        }
    }

    /// Changing what a session persists needs SCHEMA_REVISION + 1 and a new
    /// hash here; otherwise an older CLI would silently drop the new field.
    #[test]
    fn persisted_session_shape_matches_the_schema_revision() {
        use sha2::{Digest, Sha256};
        fn shape(value: &mut serde_json::Value) {
            match value {
                serde_json::Value::Object(map) => {
                    map.remove("description");
                    map.remove("title");
                    map.values_mut().for_each(shape);
                }
                serde_json::Value::Array(items) => items.iter_mut().for_each(shape),
                _ => {}
            }
        }
        let mut schema = serde_json::to_value(schemars::schema_for!(Session)).unwrap();
        shape(&mut schema);
        let schema = schema.to_string();
        let hash: String = Sha256::digest(schema.as_bytes())[..8]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert_eq!(
            (SCHEMA_REVISION, hash.as_str()),
            (0, "5ad912e6be70aa80"),
            "the persisted shape of Session changed (doc comments do not count): bump SCHEMA_REVISION and update this hash"
        );
    }

    #[test]
    fn held_lock_is_e_locked_and_retryable() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        store.create(session("local/abc/x")).unwrap();
        let lock_path = store.state_dir().lock_file("local/abc/x").unwrap();
        let file = File::options().write(true).open(&lock_path).unwrap();
        let mut lock = fd_lock::RwLock::new(file);
        let _held = lock.write().unwrap();
        let err = store.update("local/abc/x", |_| Ok(())).unwrap_err();
        assert_eq!(err.code, ErrorCode::ELocked);
        assert!(err.retryable);
    }

    #[test]
    fn list_walks_nested_ids_newest_first() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        store.create(session("local/abc/x")).unwrap();
        std::thread::sleep(Duration::from_millis(5));
        store.create(session("pr/github.com/o/r/7")).unwrap();
        let ids: Vec<String> = store.list().unwrap().into_iter().map(|s| s.id).collect();
        assert_eq!(ids, vec!["pr/github.com/o/r/7", "local/abc/x"]);
    }

    #[test]
    fn list_skips_a_broken_session_file() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        store.create(session("local/abc/x")).unwrap();
        let broken = store.state_dir().session_file("local/abc/broken").unwrap();
        std::fs::create_dir_all(broken.parent().unwrap()).unwrap();
        std::fs::write(&broken, b"{ not json").unwrap();
        let ids: Vec<String> = store.list().unwrap().into_iter().map(|s| s.id).collect();
        assert_eq!(ids, vec!["local/abc/x"]);
    }

    #[test]
    fn open_or_create_creates_then_resumes() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        let (first, ()) = store
            .open_or_create("local/abc/x", || Ok(session("local/abc/x")), |_| Ok(()))
            .unwrap();
        assert_eq!(first.rev, 1);
        let (second, n) = store
            .open_or_create(
                "local/abc/x",
                || panic!("must not init an existing session"),
                |s| Ok(s.comments.len()),
            )
            .unwrap();
        assert_eq!(second.rev, 2);
        assert_eq!(n, 0);
    }

    #[test]
    fn remove_deletes_the_session_directory() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        store.create(session("local/abc/x")).unwrap();
        store.remove("local/abc/x").unwrap();
        assert!(!store.exists("local/abc/x"));
        assert_eq!(store.remove("local/abc/x").unwrap_err().code, ErrorCode::ENoSession);
    }
}
