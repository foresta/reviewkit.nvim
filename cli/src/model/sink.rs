use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::Reason;
use super::session::ThreadActionKind;
use crate::error::Error;

/// `consume`, `comment_ids` and `expect_digest` are the keys every sink shares; the rest of
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
    /// The `digest` of the preview the user confirmed; a send that differs
    /// from it is refused instead of posting something not shown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expect_digest: Option<String>,
    #[serde(flatten)]
    pub sink: serde_json::Map<String, serde_json::Value>,
}

impl SubmitOptions {
    pub fn sink_options<T: serde::de::DeserializeOwned>(&self) -> crate::error::Result<T> {
        super::strict(serde_json::Value::Object(self.sink.clone()), "sink option")
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
/// `failed`, `skipped`, `thread_actions` and `decision`, and may fill `url`,
/// `mode`, `attempted` and `decision_version`; `at` and `summary` default to
/// empty and are filled by core.
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
    /// What `SubmitOptions.expect_digest` is compared with; only sinks that
    /// can check it under the session lock give one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest: Option<String>,
    /// Comments an interrupted submit is found to have posted already. They
    /// are not sent again; confirming the preview records them as sent.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recovered: Vec<String>,
    /// The interrupted submit's review is found to have carried the decision;
    /// confirming the preview records it as sent.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub recovered_decision: bool,
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

/// `cli` sinks are built into this binary and `submit --sink` sends; `lua`
/// sinks are registered in the client and report through `submit apply`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum SinkRuntime {
    Cli,
    Lua,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct SinkInfo {
    pub name: String,
    pub available: bool,
    pub reason: Option<String>,
    pub runtime: SinkRuntime,
}

impl SinkResult {
    /// For `submit apply` only: a client sink reports after delivering, so a
    /// value this build does not know must not refuse the whole report (the
    /// `sent` entries would go unrecorded and be delivered again). Each one is
    /// rewritten to a value that records no more than certainly happened. The
    /// CLI's own sinks and stored sessions keep the strict types.
    pub fn degrade_unknown_values(raw: &mut serde_json::Value) {
        let Some(map) = raw.as_object_mut() else { return };
        let sink = map.get("sink").and_then(|s| s.as_str()).unwrap_or("").to_string();
        // Not `sent`: that consumes the comment and stops the sink resending
        // it, though an unknown mode may only have reached a register.
        degrade::<crate::model::DeliveryMode>(map.get_mut("mode"), "copied", "mode");
        // Not `sent`: the decision stays queued rather than being cleared
        // without proof it arrived.
        degrade::<DecisionStatus>(map.get_mut("decision"), "not_applicable", "decision");
        for failed in array(map.get_mut("failed")) {
            if let Some(entry) = failed.as_object_mut() {
                let error = entry.entry("error").or_insert(serde_json::Value::Null);
                degrade_error(error, &sink);
            }
        }
        for skipped in array(map.get_mut("skipped")) {
            degrade::<Reason>(skipped.get_mut("reason"), "not_supported", "skipped[].reason");
        }
        if let Some(serde_json::Value::Array(actions)) = map.get_mut("thread_actions") {
            // No kind to record it under; left out, the action stays queued.
            actions.retain(|a| {
                let known = a.get("action").is_some_and(is_known::<ThreadActionKind>);
                if !known {
                    log::warn!(
                        "dropped a thread action of unknown kind: {}",
                        a.get("action").unwrap_or(&serde_json::Value::Null)
                    );
                }
                known
            });
            for action in actions.iter_mut() {
                degrade::<SendStatus>(action.get_mut("status"), "failed", "thread_actions[].status");
                if let Some(error) = action.get_mut("error").filter(|e| !e.is_null()) {
                    degrade_error(error, &sink);
                }
            }
        }
    }
}

fn array(value: Option<&mut serde_json::Value>) -> impl Iterator<Item = &mut serde_json::Value> {
    value.and_then(|v| v.as_array_mut()).into_iter().flatten()
}

fn is_known<T: serde::de::DeserializeOwned>(value: &serde_json::Value) -> bool {
    serde_json::from_value::<T>(value.clone()).is_ok()
}

fn degrade<T: serde::de::DeserializeOwned>(value: Option<&mut serde_json::Value>, fallback: &str, field: &str) {
    if let Some(value) = value.filter(|v| !is_known::<T>(v)) {
        log::warn!("unknown {field} {value} read as {fallback}");
        *value = serde_json::Value::String(fallback.into());
    }
}

fn degrade_error(error: &mut serde_json::Value, sink: &str) {
    if is_known::<Error>(error) {
        return;
    }
    let field = |key: &str| error.get(key).cloned().unwrap_or(serde_json::Value::Null);
    let original = field("code");
    let message = match (field("message"), error.as_str()) {
        (serde_json::Value::String(m), _) => m,
        (_, Some(m)) => m.to_string(),
        _ => format!("{sink} reported a failure without a message"),
    };
    let known_code = serde_json::from_value::<crate::error::ErrorCode>(original.clone()).ok();
    let mut degraded = Error::new(known_code.unwrap_or(crate::error::ErrorCode::ETool), message);
    // Not dropped with the broken fields: they are all that diagnoses the sink.
    let mut details = match field("details") {
        serde_json::Value::Object(map) => map,
        _ => serde_json::Map::new(),
    };
    if known_code.is_none() {
        if let serde_json::Value::String(code) = &original {
            degraded.message = format!("{code}: {}", degraded.message);
        }
        let key = if details.contains_key("code") {
            "reported_code"
        } else {
            "code"
        };
        details.insert(key.into(), original);
    }
    degraded = degraded.details(serde_json::Value::Object(details));
    degraded.tool = field("tool")
        .as_str()
        .map(str::to_string)
        .or_else(|| Some(sink.to_string()).filter(|s| !s.is_empty()));
    degraded.hint = field("hint").as_str().map(str::to_string);
    degraded.retryable = field("retryable").as_bool().unwrap_or(false);
    log::warn!(
        "unrecognised error from sink {sink} read as {}: {}",
        degraded.code,
        degraded.message
    );
    *error = serde_json::to_value(degraded).unwrap_or(serde_json::Value::Null);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn degraded(mut error: serde_json::Value) -> serde_json::Value {
        degrade_error(&mut error, "clipboard");
        error
    }

    #[test]
    fn a_known_code_beside_broken_fields_keeps_the_sinks_details() {
        let error = degraded(serde_json::json!({"code": "E_TOOL", "retryable": "yes", "details": {"register": "+"}}));
        assert_eq!(error["code"], "E_TOOL");
        assert_eq!(error["details"]["register"], "+", "{error}");
    }

    #[test]
    fn an_unknown_code_does_not_overwrite_the_sinks_own_details_code() {
        let error = degraded(serde_json::json!({
            "code": "E_CLIPBOARD",
            "message": "busy",
            "details": {"code": "EBUSY", "register": "+"},
        }));
        assert_eq!(error["code"], "E_TOOL");
        assert_eq!(error["details"]["code"], "EBUSY", "{error}");
        assert_eq!(error["details"]["reported_code"], "E_CLIPBOARD", "{error}");
        assert_eq!(error["details"]["register"], "+");
    }

    #[test]
    fn an_unknown_code_goes_to_details_code_when_the_sink_gave_none() {
        let error = degraded(serde_json::json!({"code": "E_CLIPBOARD", "message": "busy"}));
        assert_eq!(error["details"]["code"], "E_CLIPBOARD", "{error}");
        assert!(error["details"].get("reported_code").is_none());
    }
}
