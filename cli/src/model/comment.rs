use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::Side;
use crate::error::ErrorCode;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Comment {
    pub id: String,
    pub path: String,
    pub side: Side,
    pub line: u32,
    pub start_line: Option<u32>,
    pub kind: CommentKind,
    pub body: String,
    pub suggestion: Option<String>,
    pub reply_to: Option<String>,
    pub anchor: Anchor,
    pub stale: bool,
    pub commentable: Commentable,
    pub last_error: Option<LastError>,
    /// Where this text already went. Cleared when the body or suggestion
    /// changes: a delivery of the old text says nothing about the new one.
    #[serde(default)]
    pub deliveries: Vec<Delivery>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Delivery {
    pub sink: String,
    pub at: String,
    pub mode: DeliveryMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// This delivery took the comment out of the queue. When the sink is the
    /// session's final destination the comment is not put back (an edit
    /// would deliver it twice); the history drops these first.
    #[serde(default)]
    pub consumed: bool,
}

/// `staged` text was only typed into an agent's input and may be thrown
/// away there; `copied` only reached a register. Neither consumes a comment.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum DeliveryMode {
    #[default]
    Sent,
    Staged,
    Copied,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum CommentKind {
    #[default]
    Issue,
    Suggestion,
    Question,
    Praise,
    Nit,
}

/// Where the comment was made, kept so a later refresh can tell whether the
/// line moved. `head_sha` is `null` on a working tree; `blob` pins it then.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Anchor {
    pub base_sha: String,
    pub head_sha: Option<String>,
    pub blob: Option<String>,
    pub context: AnchorContext,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AnchorContext {
    pub before: Vec<String>,
    pub lines: Vec<String>,
    pub after: Vec<String>,
}

/// Whether GitHub would accept the comment; `reason` says why not
/// (`outside_diff`, or `binary`, which is independent of the host). The
/// other reasons only appear in `Skipped`. Comments outside the diff are
/// still queued (D4); the sink decides what to skip.
/// Named after GitHub rather than a neutral `in_diff` because the ranges,
/// this flag and `outside_diff` all follow GitHub's rule; a second hosting
/// sink needs per-destination ranges and reasons, a version bump anyway.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Commentable {
    pub github: bool,
    pub reason: Option<Reason>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    OutsideDiff,
    Stale,
    Binary,
    NotSupported,
    Delivered,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LastError {
    pub sink: String,
    pub code: ErrorCode,
    pub message: String,
    pub at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CommentInput {
    pub path: String,
    pub side: Side,
    pub line: u32,
    #[serde(default)]
    pub start_line: Option<u32>,
    #[serde(default)]
    pub kind: CommentKind,
    pub body: String,
    #[serde(default)]
    pub suggestion: Option<String>,
    #[serde(default)]
    pub reply_to: Option<String>,
}

/// Omitted = unchanged, `null` = cleared, value = set. Plain `Option` cannot
/// tell omitted from `null`, so the clearable fields are `Option<Option<T>>`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct CommentPatch {
    #[serde(default)]
    pub line: Option<u32>,
    #[serde(
        default,
        deserialize_with = "some_if_present",
        skip_serializing_if = "Option::is_none"
    )]
    pub start_line: Option<Option<u32>>,
    #[serde(default)]
    pub kind: Option<CommentKind>,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(
        default,
        deserialize_with = "some_if_present",
        skip_serializing_if = "Option::is_none"
    )]
    pub suggestion: Option<Option<String>>,
}

fn some_if_present<'de, D, T>(deserializer: D) -> std::result::Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patch_distinguishes_omitted_null_and_value() {
        let omitted: CommentPatch = serde_json::from_str("{}").unwrap();
        assert_eq!(omitted.suggestion, None);
        let cleared: CommentPatch = serde_json::from_str(r#"{"suggestion": null}"#).unwrap();
        assert_eq!(cleared.suggestion, Some(None));
        let set: CommentPatch = serde_json::from_str(r#"{"suggestion": "x", "start_line": 3}"#).unwrap();
        assert_eq!(set.suggestion, Some(Some("x".into())));
        assert_eq!(set.start_line, Some(Some(3)));
    }
}
