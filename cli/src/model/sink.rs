use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::Reason;
use super::session::ThreadActionKind;
use crate::error::Error;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct SubmitOptions {
    #[serde(default)]
    pub target: Option<String>,
    #[serde(default)]
    pub agent_type: Option<AgentType>,
    #[serde(default)]
    pub header: Option<String>,
    #[serde(default)]
    pub stage: Option<bool>,
    #[serde(default)]
    pub match_foreground_cwd: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum AgentType {
    Claude,
    Plain,
}

/// Also the stdin of `submit apply`: a Lua sink fills `sink`, `sent`,
/// `failed`, `skipped`, `thread_actions` and `decision`; `at` and `summary`
/// default to empty and are filled by core.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SinkResult {
    pub sink: String,
    #[serde(default)]
    pub at: String,
    #[serde(default)]
    pub url: Option<String>,
    pub sent: Vec<Sent>,
    pub failed: Vec<Failed>,
    pub skipped: Vec<Skipped>,
    #[serde(default)]
    pub thread_actions: Vec<ThreadActionResult>,
    pub decision: DecisionStatus,
    #[serde(default)]
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Sent {
    pub comment_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

/// `comment_id` is `null` when the whole review failed rather than one comment.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Failed {
    pub comment_id: Option<String>,
    pub error: Error,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Skipped {
    pub comment_id: String,
    pub reason: Reason,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ThreadActionResult {
    pub thread_id: String,
    pub action: ThreadActionKind,
    pub status: SendStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<Error>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum SendStatus {
    Sent,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DecisionStatus {
    Sent,
    Failed,
    Skipped,
    NotApplicable,
}
