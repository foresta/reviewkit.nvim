use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::model::{Etags, Session};

/// Hashes the JSON serde emits for one section (field order is the struct's
/// declaration order, maps are `BTreeMap`), so the etag is tied to what the
/// client actually receives rather than to an in-memory `Hash`.
pub fn etag(section: &impl Serialize) -> String {
    let bytes = serde_json::to_vec(section).unwrap_or_default();
    let digest = Sha256::digest(&bytes);
    // 16 hex chars (64 bits) are plenty for change detection and keep the
    // envelope small.
    digest[..8].iter().map(|b| format!("{b:02x}")).collect()
}

pub fn compute(session: &Session) -> Etags {
    Etags {
        targets: etag(&session.targets),
        comments: etag(&session.comments),
        decision: etag(&session.decision),
        threads: etag(&session.threads),
        thread_actions: etag(&session.thread_actions),
        viewed: etag(&session.viewed),
    }
}
