use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::Reason;
use super::session::ThreadActionKind;
use crate::error::Error;

/// `consume` and `comment_ids` are the keys every sink shares; the rest of
/// the object is the chosen sink's own options (`HerdrOptions`,
/// `GithubOptions`), kept flat so a client can pass one table per sink as it is.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct SubmitOptions {
    /// Whether delivered comments leave the queue. By default only the
    /// session kind's own destination consumes them; github always does,
    /// since keeping posted comments would post them again.
    #[serde(default)]
    pub consume: Option<bool>,
    /// Sends only these queued comments, without the decision and thread
    /// actions. An id already in the history counts as sent and is dropped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment_ids: Option<Vec<String>>,
    #[serde(flatten)]
    pub sink: serde_json::Map<String, serde_json::Value>,
}

impl SubmitOptions {
    pub fn sink_options<T: serde::de::DeserializeOwned>(&self) -> crate::error::Result<T> {
        serde_json::from_value(serde_json::Value::Object(self.sink.clone()))
            .map_err(|e| crate::error::Error::invalid(format!("invalid sink options: {e}")))
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct HerdrOptions {
    /// A pane id (`SinkTarget.id`); without it the pane is found by cwd.
    #[serde(default)]
    pub target: Option<String>,
    #[serde(default)]
    pub agent_type: Option<AgentType>,
    #[serde(default)]
    pub header: Option<String>,
    /// Puts the text in the agent's input without pressing Enter (not git
    /// staging); the agent's user submits or discards it.
    #[serde(default)]
    pub stage: Option<bool>,
    #[serde(default)]
    pub match_foreground_cwd: Option<bool>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct GithubOptions {}

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
    /// `Decision.updated_at` as the sink read it; a decision set again
    /// while the sink ran is neither recorded as delivered nor cleared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision_version: Option<String>,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub mode: crate::model::DeliveryMode,
    /// `None` means every queued comment; given, a whole failure marks only
    /// these, so comments added while the sink was sending stay unmarked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempted: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Sent {
    pub comment_id: String,
    /// The comment's `updated_at` when its text was handed to the sink. A
    /// comment edited since then was not what got delivered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
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

/// What a submit would do, in the same shape for every sink. Comments are
/// named by id; a client finds their path and text in its session.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SubmitPreview {
    pub sink: String,
    /// Comments placed on their lines.
    pub comment_ids: Vec<String>,
    /// Comments written into `text` because the destination cannot place
    /// them on a line.
    pub body_comment_ids: Vec<String>,
    pub skipped: Vec<Skipped>,
    pub replies: Vec<PreviewReply>,
    pub thread_actions: Vec<super::ThreadAction>,
    /// The review event that will be posted, also when it is implied (a
    /// github review of comments without a Decision is a `comment`).
    pub decision: Option<super::DecisionEvent>,
    /// The text sent as a whole: the review body (github) or the prompt (herdr).
    pub text: Option<String>,
    /// Where it goes when the sink picks a destination (a herdr pane).
    pub target: Option<String>,
    pub mode: super::DeliveryMode,
    /// The sink's own request, for debugging. Not part of the contract: it
    /// changes whenever the sink's transport does.
    #[schemars(with = "serde_json::Map<String, serde_json::Value>")]
    pub raw: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PreviewReply {
    pub comment_id: String,
    pub thread_id: String,
}

/// A destination a sink can send to, for a picker (`sink targets`, and the
/// candidates of `E_AMBIGUOUS`).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SinkTarget {
    /// What `HerdrOptions.target` takes.
    pub id: String,
    pub name: Option<String>,
    pub status: Option<String>,
    pub cwd: Option<String>,
    pub title: Option<String>,
}
