use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Kept in `pr.json`, not in the session, so the envelope stays small.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PrInfo {
    pub title: String,
    pub body: String,
    pub author: String,
    pub url: String,
    pub created_at: String,
    pub updated_at: String,
    pub timeline: Vec<TimelineItem>,
    pub checks: Vec<Check>,
    pub fetched_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TimelineItem {
    pub kind: TimelineKind,
    pub author: String,
    pub body: Option<String>,
    pub state: Option<String>,
    pub created_at: String,
    pub url: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum TimelineKind {
    Comment,
    Review,
    Commit,
    Event,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Check {
    pub name: String,
    pub status: String,
    pub conclusion: Option<String>,
    pub url: Option<String>,
}
