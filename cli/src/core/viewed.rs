use crate::core::time;
use crate::error::{Error, ErrorCode, Result};
use crate::model::{Session, Viewed};

/// Records the head blob, not just the path, so the mark expires by itself
/// when the file changes. A deleted file has no head blob; the empty string
/// stands in so it can still be marked.
pub fn set(session: &mut Session, path: &str, on: bool) -> Result<()> {
    let target = session
        .targets
        .iter()
        .find(|t| t.path == path)
        .ok_or_else(|| Error::new(ErrorCode::ENoTarget, format!("no such target: {path}")))?;
    if on {
        let blob = target.head_blob.clone().unwrap_or_default();
        session
            .viewed
            .insert(path.to_string(), Viewed { blob, at: time::now() });
    } else {
        session.viewed.remove(path);
    }
    refresh_flags(session);
    Ok(())
}

pub fn refresh_flags(session: &mut Session) {
    for target in &mut session.targets {
        target.viewed = session
            .viewed
            .get(&target.path)
            .is_some_and(|v| v.blob == target.head_blob.as_deref().unwrap_or(""));
    }
}
