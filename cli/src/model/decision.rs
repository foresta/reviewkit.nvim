use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Decision {
    pub event: DecisionEvent,
    pub body: String,
    pub updated_at: String,
    /// Where this decision went, like `Comment.deliveries`; a new decision
    /// starts with none.
    #[serde(default)]
    pub deliveries: Vec<super::Delivery>,
}

/// stdin of `decision set`; core adds `updated_at`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DecisionInput {
    pub event: DecisionEvent,
    #[serde(default)]
    pub body: String,
}

/// 1:1 with GitHub's PullRequestReviewEvent, lowercased.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DecisionEvent {
    Approve,
    RequestChanges,
    Comment,
}
