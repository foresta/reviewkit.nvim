use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::Hunk;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Target {
    pub id: String,
    pub path: String,
    pub old_path: Option<String>,
    pub status: TargetStatus,
    pub reason: TargetReason,
    pub base_blob: Option<String>,
    pub head_blob: Option<String>,
    pub binary: bool,
    pub too_large: bool,
    /// Derived: `Session.viewed[path].blob == head_blob`. Stored so clients
    /// never recompute it.
    pub viewed: bool,
    pub stats: Stats,
    pub hunks: Vec<Hunk>,
    pub commentable: CommentableRanges,
    pub range: Option<Range>,
}

/// git's own status letters spelled out, so `git diff --name-status` maps 1:1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum TargetStatus {
    Modified,
    Added,
    Deleted,
    Renamed,
    Copied,
    Type,
    Untracked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum TargetReason {
    Changed,
    Thread,
    Impact,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Stats {
    pub added: u32,
    pub removed: u32,
}

/// Closed 1-based line ranges, merged and clipped to the file, per side.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CommentableRanges {
    pub base: Vec<[u32; 2]>,
    pub head: Vec<[u32; 2]>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Range {
    pub start: u32,
    pub end: u32,
}
