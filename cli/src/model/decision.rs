use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Decision {
    pub event: DecisionEvent,
    pub body: String,
    pub updated_at: String,
}

/// 1:1 with GitHub's PullRequestReviewEvent, lowercased.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DecisionEvent {
    Approve,
    RequestChanges,
    Comment,
}
