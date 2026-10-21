mod state_dir;

pub use state_dir::StateDir;

use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::core::{etags, time};
use crate::error::{Error, ErrorCode, Result};
use crate::model::{SCHEMA_REVISION, SCHEMA_VERSION, Session, SinkResult};

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
        let (mut session, _) = read_session(&path, id)?;
        self.mark_pending(&mut session);
        Ok(session)
    }

    /// For previews: the next write applies queued results first, so a plan
    /// made without them would never match the send.
    pub fn load_with_queued(&self, id: &str) -> Result<Session> {
        let mut session = self.load(id)?;
        self.drain_applies(&mut session, false);
        Ok(session)
    }

    fn mark_pending(&self, session: &mut Session) {
        session.pending_submit = self
            .dir
            .session_dir(&session.id)
            .map(|dir| dir.join(PENDING_SUBMIT_FILE).exists())
            .unwrap_or(false);
        // A result a newer CLI queued counts too, though only that CLI can
        // record it: it is a delivery another instance made, and leaving it
        // out would let a Lua sink deliver the same comments again.
        session.pending_apply = !self.queued_applies(&session.id).is_empty();
    }

    /// A client sink's result is on disk before the lock is waited for: the
    /// delivery already happened, and a result lost to a busy lock would be
    /// delivered again.
    pub fn queue_apply(&self, id: &str, queued: &QueuedApply) -> Result<()> {
        if !self.exists(id) {
            return Err(no_session(id));
        }
        let dir = self.dir.session_dir(id)?.join(APPLIES_DIR);
        // Not create_dir_all: a session purged meanwhile must not come back
        // as a directory holding only this file.
        match std::fs::create_dir(&dir) {
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Err(no_session(id)),
            Err(err) if err.kind() != std::io::ErrorKind::AlreadyExists => return Err(err.into()),
            _ => {}
        }
        let bytes = serde_json::to_vec_pretty(queued).map_err(serialize_error)?;
        write_atomically(&dir.join(format!("{}.json", ulid::Ulid::generate())), &bytes)
    }

    fn queued_applies(&self, id: &str) -> Vec<PathBuf> {
        let Ok(dir) = self.dir.session_dir(id) else {
            return Vec::new();
        };
        let Ok(entries) = std::fs::read_dir(dir.join(APPLIES_DIR)) else {
            return Vec::new();
        };
        let mut files: Vec<PathBuf> = entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.is_file() && p.extension().is_some_and(|x| x == "json"))
            .collect();
        files.sort();
        files
    }

    /// A file that cannot be read is set aside rather than failing a write
    /// that may be recording a GitHub post.
    fn drain_applies(&self, session: &mut Session, set_aside_bad: bool) -> Vec<PathBuf> {
        let mut applied = Vec::new();
        for path in self.queued_applies(&session.id) {
            let queued = std::fs::read(&path)
                .ok()
                .and_then(|bytes| serde_json::from_slice::<QueuedApply>(&bytes).ok());
            match queued {
                // Fields a newer CLI added would be dropped by applying it here.
                Some(queued) if queued.revision > SCHEMA_REVISION => {}
                Some(queued) => match crate::core::apply::apply(session, queued.result, queued.consume) {
                    Ok(_) => applied.push(path),
                    Err(err) if set_aside_bad => set_aside(&path, &err.to_string()),
                    Err(_) => {}
                },
                None if set_aside_bad => set_aside(&path, "unreadable"),
                None => {}
            }
        }
        applied
    }

    /// For a caller about to do slow work before its write (a PR fetch):
    /// a session this CLI may not write fails here rather than after it.
    pub fn load_for_write(&self, id: &str) -> Result<Session> {
        let path = self.dir.session_file(id)?;
        match read_session(&path, id)? {
            (mut session, None) => {
                self.mark_pending(&mut session);
                Ok(session)
            }
            (_, Some(writer)) => Err(newer_writer(&path, &writer)),
        }
    }

    /// Sessions are created through `open_or_create`; this seeds tests.
    #[cfg(test)]
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
        self.update_then(id, apply, || {})
    }

    /// `after_commit` runs once the session is saved and before the lock is
    /// released: a record that may only go once the save is done (a pending
    /// submit) would otherwise be removed after another writer wrote its own.
    pub fn update_then<T>(
        &self,
        id: &str,
        apply: impl FnOnce(&mut Session) -> Result<T>,
        after_commit: impl FnOnce(),
    ) -> Result<(Session, T)> {
        self.with_lock(id, || {
            let mut session = self.load_for_write(id)?;
            let drained = self.drain_applies(&mut session, true);
            let value = apply(&mut session)?;
            self.commit(&mut session)?;
            remove_applied(&drained);
            after_commit();
            self.mark_pending(&mut session);
            Ok((session, value))
        })
    }

    /// `update` that leaves the file alone when `apply` changed nothing. For
    /// refreshes: the client watches the file, and a write that changes
    /// nothing would make it refresh again.
    pub fn update_if_changed<T>(
        &self,
        id: &str,
        apply: impl FnOnce(&mut Session) -> Result<T>,
    ) -> Result<(Session, T)> {
        self.with_lock(id, || {
            let mut session = self.load_for_write(id)?;
            let before = serde_json::to_value(&session).map_err(serialize_error)?;
            let drained = self.drain_applies(&mut session, true);
            let value = apply(&mut session)?;
            if serde_json::to_value(&session).map_err(serialize_error)? != before {
                self.commit(&mut session)?;
                remove_applied(&drained);
            } else {
                // commit sets it on writes; a moved state dir must still
                // hand the client the file it can watch.
                session.state_path = self.dir.session_file(id)?;
            }
            self.mark_pending(&mut session);
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
            let mut drained = Vec::new();
            let mut session = if self.exists(id) {
                let mut session = self.load_for_write(id)?;
                drained = self.drain_applies(&mut session, true);
                session
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
            remove_applied(&drained);
            self.mark_pending(&mut session);
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
        lock_while(&self.dir.lock_file(id)?, self.lock_timeout, &format!("session {id}"), f).map_err(|err| {
            if err.code == ErrorCode::ELocked {
                err.hint("a submit may still be running; try again when it finishes")
            } else {
                err
            }
        })
    }

    /// `with_fetch_lock` for a session that may not exist yet: its directory
    /// has to be there before the lock file can be. A first open that fails
    /// before its first save takes the directory away again, still holding
    /// the lock: after releasing it, a waiting open would already be writing
    /// its pr.json there. That waiter then holds a lock on a removed file, so
    /// it makes the directory again and locks anew.
    pub fn with_new_fetch_lock<T>(&self, id: &str, f: impl FnOnce() -> Result<T>) -> Result<T> {
        let dir = self.dir.session_dir(id)?;
        let lock_path = self.dir.fetch_lock_file(id)?;
        let mut f = Some(f);
        loop {
            let created = !dir.exists();
            std::fs::create_dir_all(&dir)?;
            // A failing first open may remove the directory between the two.
            let file = match open_lock_file(&lock_path) {
                Ok(file) => file,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
                Err(err) => return Err(err.into()),
            };
            let outcome = lock_file_while(file, FETCH_LOCK_TIMEOUT, &format!("session {id}"), |file| {
                if !still_at(file, &lock_path) {
                    return Ok(None);
                }
                let Some(f) = f.take() else {
                    return Err(Error::internal("the fetch ran twice"));
                };
                let result = f();
                if result.is_err() && created && !self.exists(id) {
                    let _ = std::fs::remove_dir_all(&dir);
                    self.remove_empty_parents(&dir);
                }
                Ok(Some(result))
            })?;
            if let Some(result) = outcome {
                return result;
            }
        }
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

/// Written by the GitHub sink before it sends; the store only reports
/// whether it is there.
pub const PENDING_SUBMIT_FILE: &str = "pending_submit.json";

pub const APPLIES_DIR: &str = "applies";

#[derive(Debug, Serialize, Deserialize)]
pub struct QueuedApply {
    pub revision: u32,
    pub consume: bool,
    pub result: SinkResult,
}

fn set_aside(path: &Path, why: &str) {
    log::warn!("setting aside the queued result {}: {why}", path.display());
    if let (Some(dir), Some(name)) = (path.parent(), path.file_name()) {
        let rejected = dir.join("rejected");
        if std::fs::create_dir_all(&rejected).is_ok() && std::fs::rename(path, rejected.join(name)).is_ok() {
            return;
        }
    }
    let _ = std::fs::remove_file(path);
}

/// After the save: a crash before this replays them, which records nothing
/// twice (`apply` skips a delivery it already has).
fn remove_applied(paths: &[PathBuf]) {
    for path in paths {
        let _ = std::fs::remove_file(path);
    }
}

pub const FETCH_LOCK_TIMEOUT: Duration = Duration::from_secs(180);

/// Runs `f` while holding an advisory lock on `path`. Polls instead of
/// blocking so a stuck holder turns into E_LOCKED rather than a hang.
pub fn lock_while<T>(path: &Path, timeout: Duration, what: &str, f: impl FnOnce() -> Result<T>) -> Result<T> {
    let file = open_lock_file(path)?;
    lock_file_while(file, timeout, what, |_| f())
}

fn open_lock_file(path: &Path) -> std::io::Result<File> {
    File::options().create(true).truncate(false).write(true).open(path)
}

fn lock_file_while<T>(file: File, timeout: Duration, what: &str, f: impl FnOnce(&File) -> Result<T>) -> Result<T> {
    use std::io::ErrorKind::{Interrupted, WouldBlock};
    let mut lock = fd_lock::RwLock::new(file);
    let deadline = Instant::now() + timeout;
    loop {
        match lock.try_write() {
            Ok(guard) => return f(&guard),
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

#[cfg(unix)]
fn still_at(file: &File, path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (file.metadata(), std::fs::metadata(path)) {
        (Ok(locked), Ok(current)) => locked.dev() == current.dev() && locked.ino() == current.ino(),
        _ => false,
    }
}

#[cfg(not(unix))]
fn still_at(_file: &File, path: &Path) -> bool {
    path.exists()
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

/// The revision that added `Delivery.consumed`.
const CONSUMED_RECORDED: u32 = 7;

/// Before that revision only a consuming delivery could move a comment to
/// the history, so every `sent` delivery there consumed it. Read as such,
/// restoring and editing one cannot post it again.
fn mark_legacy_history_consumed(raw: &mut serde_json::Value) {
    let Some(history) = raw.get_mut("history").and_then(|h| h.as_array_mut()) else {
        return;
    };
    for comment in history {
        let Some(deliveries) = comment.get_mut("deliveries").and_then(|d| d.as_array_mut()) else {
            continue;
        };
        for delivery in deliveries {
            if delivery.get("mode").and_then(|m| m.as_str()) == Some("sent") {
                delivery["consumed"] = serde_json::json!(true);
            }
        }
    }
}

type Migration = fn(&mut serde_json::Value) -> Result<()>;

/// `MIGRATIONS[n - 1]` turns a version-n session into version n+1 as it is
/// read, so bumping SCHEMA_VERSION never strands the comments a user has
/// queued. A bump without its step fails `every_schema_version_has_a_migration`.
const MIGRATIONS: &[Migration] = &[];

/// Ok(false) when no chain of steps reaches this version.
fn migrate(raw: &mut serde_json::Value, found: u64, steps: &[Migration]) -> Result<bool> {
    migrate_to(raw, found, u64::from(SCHEMA_VERSION), steps)
}

fn migrate_to(raw: &mut serde_json::Value, found: u64, target: u64, steps: &[Migration]) -> Result<bool> {
    if found == 0 || found >= target {
        return Ok(found == target);
    }
    for version in found..target {
        let Some(step) = steps.get(version as usize - 1) else {
            return Ok(false);
        };
        step(raw)?;
    }
    raw["schema_version"] = serde_json::json!(target);
    // SCHEMA_REVISION counts within one version; an old version's writer
    // compared against it would look newer and lock the session.
    if let Some(map) = raw.as_object_mut() {
        map.remove(WRITER_KEY);
    }
    Ok(true)
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
    let mut raw: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|err| corrupt(path, format!("not JSON: {err}")))?;
    let found = raw.get("schema_version").and_then(|v| v.as_u64()).unwrap_or(0);
    // Read before migrating, which drops the old writer record.
    let written_at = raw
        .get(WRITER_KEY)
        .and_then(|w| serde_json::from_value::<Writer>(w.clone()).ok())
        .unwrap_or_default()
        .revision;
    if !migrate(&mut raw, found, MIGRATIONS)? {
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
    let newer = (writer.revision > SCHEMA_REVISION).then_some(writer.clone());
    if written_at < CONSUMED_RECORDED {
        mark_legacy_history_consumed(&mut raw);
    }
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

pub fn write_atomically(path: &Path, bytes: &[u8]) -> Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| Error::internal("session file has no parent directory"))?;
    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
    tmp.write_all(bytes)?;
    tmp.as_file().sync_all()?;
    tmp.persist(path)
        .map_err(|err| Error::internal(format!("cannot replace {}: {}", path.display(), err.error)))?;
    // The file is already replaced; failing now would make the caller retry
    // a write that happened. Some file systems refuse to sync a directory.
    if let Err(err) = sync_dir(dir) {
        log::warn!("cannot sync {}: {}", dir.display(), err.message);
    }
    Ok(())
}

/// The rename lives in the directory entry; until that is on disk a power
/// loss can bring back the old file or none, which for the pending submit
/// record means posting again what already reached GitHub.
#[cfg(unix)]
fn sync_dir(dir: &Path) -> Result<()> {
    File::open(dir)?.sync_all()?;
    Ok(())
}

/// Windows cannot open a directory as a file to flush it.
#[cfg(not(unix))]
fn sync_dir(_dir: &Path) -> Result<()> {
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

    #[test]
    fn a_failed_first_open_does_not_take_away_what_a_waiting_open_wrote() {
        let dir = tempfile::tempdir().unwrap();
        let id = "pr/h/o/r/1";
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let first = {
            let store = store(&dir);
            std::thread::spawn(move || {
                store.with_new_fetch_lock(id, || -> Result<()> {
                    started_tx.send(()).unwrap();
                    std::thread::sleep(Duration::from_millis(300));
                    Err(Error::invalid("the first fetch failed"))
                })
            })
        };
        started_rx.recv().unwrap();
        let store = store(&dir);
        let pr_file = store.state_dir().pr_file(id).unwrap();
        store
            .with_new_fetch_lock(id, || {
                std::thread::sleep(Duration::from_millis(100));
                std::fs::write(&pr_file, b"{}")?;
                Ok(())
            })
            .unwrap();
        assert!(first.join().unwrap().is_err());
        assert!(pr_file.is_file(), "the waiting open keeps its pr.json");
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
                    deliveries: vec![],
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
    fn a_history_written_before_consumption_was_recorded_reads_as_consumed() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        let created = store.create(session("local/abc/legacy")).unwrap();
        let comment = serde_json::json!({
            "id": "c_1", "path": "a", "side": "head", "line": 1, "kind": "issue", "body": "b",
            "anchor": { "base_sha": "a", "head_sha": null, "blob": null,
                "context": { "before": [], "lines": [], "after": [] } },
            "stale": false, "commentable": { "github": true, "reason": null },
            "deliveries": [
                { "sink": "github", "at": "t", "mode": "sent" },
                { "sink": "clipboard", "at": "t", "mode": "copied" }
            ],
            "created_at": "t", "updated_at": "t"
        });
        for (revision, consumed) in [(6, true), (SCHEMA_REVISION, false)] {
            let mut raw: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&created.state_path).unwrap()).unwrap();
            raw["history"] = serde_json::json!([comment.clone()]);
            raw["_writer"]["revision"] = serde_json::json!(revision);
            std::fs::write(&created.state_path, serde_json::to_vec(&raw).unwrap()).unwrap();
            let loaded = store.load("local/abc/legacy").unwrap();
            let deliveries = &loaded.history[0].deliveries;
            assert_eq!(deliveries[0].consumed, consumed, "revision {revision}");
            assert!(!deliveries[1].consumed, "a copy never consumed anything");
        }
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

    #[test]
    fn every_schema_version_has_a_migration() {
        assert_eq!(MIGRATIONS.len() as u32, SCHEMA_VERSION - 1);
    }

    #[test]
    fn an_older_session_is_migrated_step_by_step_on_read() {
        fn add_note(raw: &mut serde_json::Value) -> Result<()> {
            raw["note"] = serde_json::json!("migrated");
            Ok(())
        }
        let steps: Vec<Migration> = vec![add_note; SCHEMA_VERSION as usize];
        let mut raw = serde_json::json!({ "schema_version": 0 });
        assert!(!migrate(&mut raw, 0, &steps).unwrap(), "no version 0 to start from");
        if SCHEMA_VERSION > 1 {
            let mut raw = serde_json::json!({ "schema_version": 1 });
            assert!(migrate(&mut raw, 1, &steps).unwrap());
            assert_eq!(raw["schema_version"], SCHEMA_VERSION);
            assert_eq!(raw["note"], "migrated");
            let mut raw = serde_json::json!({ "schema_version": 1 });
            assert!(!migrate(&mut raw, 1, &[]).unwrap(), "a missing step is not skipped");
        }
        if SCHEMA_VERSION > 1 {
            let mut raw = serde_json::json!({ "schema_version": 1, "_writer": { "revision": 999 } });
            assert!(migrate(&mut raw, 1, &steps).unwrap());
            assert!(raw.get("_writer").is_none(), "an old writer must not look newer");
        }
        let mut raw = serde_json::json!({ "schema_version": 1, "_writer": { "revision": 999 } });
        fn identity(_: &mut serde_json::Value) -> Result<()> {
            Ok(())
        }
        assert!(migrate_to(&mut raw, 1, 2, &[identity]).unwrap());
        assert_eq!(raw["schema_version"], 2);
        assert!(raw.get("_writer").is_none(), "an old writer must not look newer");
        let mut current = serde_json::json!({ "schema_version": SCHEMA_VERSION });
        assert!(migrate(&mut current, u64::from(SCHEMA_VERSION), &[]).unwrap());
        let mut newer = serde_json::json!({});
        assert!(!migrate(&mut newer, u64::from(SCHEMA_VERSION) + 1, &steps).unwrap());
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
        assert!(err.hint.as_deref().is_some_and(|h| h.contains("submit")));
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
    fn open_or_create_records_queued_results_when_it_resumes() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        store.create(session("local/abc/x")).unwrap();
        let result: SinkResult = serde_json::from_value(serde_json::json!({
            "sink": "slack", "at": "2026-01-01T00:00:00Z",
            "sent": [], "failed": [], "skipped": [], "decision": "not_applicable"
        }))
        .unwrap();
        store
            .queue_apply(
                "local/abc/x",
                &QueuedApply {
                    revision: SCHEMA_REVISION,
                    consume: true,
                    result,
                },
            )
            .unwrap();
        let (resumed, ()) = store
            .open_or_create("local/abc/x", || panic!("exists"), |_| Ok(()))
            .unwrap();
        assert_eq!(resumed.last_submit.map(|last| last.sink), Some("slack".to_string()));
        assert!(!resumed.pending_apply);
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

    #[test]
    fn after_commit_runs_once_saved_and_still_under_the_lock() {
        let dir = tempfile::tempdir().unwrap();
        let writer = store(&dir);
        writer.create(session("local/abc/x")).unwrap();
        let other = store(&dir);
        let mut seen = None;
        writer
            .update_then(
                "local/abc/x",
                |_| Ok(()),
                || {
                    let saved = other.load("local/abc/x").unwrap().rev;
                    let locked = other.update("local/abc/x", |_| Ok(())).unwrap_err().code;
                    seen = Some((saved, locked));
                },
            )
            .unwrap();
        assert_eq!(seen, Some((2, ErrorCode::ELocked)));
    }
}
