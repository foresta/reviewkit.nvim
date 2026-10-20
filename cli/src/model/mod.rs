pub mod comment;
pub mod decision;
pub mod health;
pub mod hunk;
pub mod pr_info;
pub mod session;
pub mod sink;
pub mod target;
pub mod thread;

pub use comment::{
    Anchor, AnchorContext, Comment, CommentInput, CommentKind, CommentPatch, Commentable, Delivery, DeliveryMode,
    LastError, Reason,
};
pub use decision::{Decision, DecisionEvent, DecisionInput};
pub use health::{HealthInfo, StateDirStatus, ToolStatus, ToolsStatus};
pub use hunk::Hunk;
pub use pr_info::{Check, PrInfo, TimelineItem, TimelineKind};
pub use session::{
    Etags, HeadRef, LastSubmit, PrState, Repo, RevRef, Session, SessionSummary, Side, Source, ThreadAction,
    ThreadActionKind, Viewed,
};
pub use sink::{
    AgentType, DecisionStatus, Failed, GithubOptions, HerdrOptions, PreviewReply, SendStatus, Sent, SinkResult,
    SinkTarget, Skipped, SubmitOptions, SubmitPreview, ThreadActionResult,
};
pub use target::{CommentableRanges, Range, Stats, Target, TargetReason, TargetStatus};
pub use thread::{SubjectType, Thread, ThreadComment};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Bumped only for incompatible changes. Within one version, fields and enum
/// values are only ever added.
pub const SCHEMA_VERSION: u32 = 1;

/// Bumped whenever the contract schema changes, inputs included (enforced by
/// tests/schema.rs). A client needs at least its own revision, or the CLI
/// would reject what it sends; a CLI does not write a session last written
/// by a higher revision, since it would drop what it cannot represent.
/// Recorded outside the contract, in `_writer`.
pub const SCHEMA_REVISION: u32 = 10;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct VersionInfo {
    pub version: String,
    pub schema_version: u32,
    /// The contract revision; a client needs at least its own.
    #[serde(default)]
    pub schema_revision: u32,
}

/// Root of the JSON Schema. It only exists so that one `schema_for!` call
/// emits every contract type under `$defs`.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct Contract {
    pub version: VersionInfo,
    pub health: HealthInfo,
    pub error: crate::error::Error,
    pub session: Session,
    pub session_summary: SessionSummary,
    pub comment_input: CommentInput,
    pub comment_patch: CommentPatch,
    pub decision_input: DecisionInput,
    pub pr_info: PrInfo,
    pub submit_options: SubmitOptions,
    pub herdr_options: HerdrOptions,
    pub github_options: GithubOptions,
    pub sink_result: SinkResult,
    pub sink_info: crate::sink::SinkInfo,
    pub submit_preview: SubmitPreview,
    pub sink_target: SinkTarget,
}

/// Inputs are strict where stored files are lenient: an unknown key means
/// the client speaks a newer contract (or has a typo), and acting without
/// it could send something other than what was asked. Types stay lenient
/// because the same types read session files an older or newer CLI wrote.
pub fn strict<T: serde::de::DeserializeOwned>(value: serde_json::Value, what: &str) -> crate::error::Result<T> {
    let mut unknown = Vec::new();
    let parsed: std::result::Result<T, _> = serde_ignored::deserialize(value, |path| unknown.push(path.to_string()));
    let parsed = parsed.map_err(|err| crate::error::Error::invalid(format!("invalid {what}: {err}")))?;
    if !unknown.is_empty() {
        return Err(crate::error::Error::invalid(format!("unknown {what} field: {}", unknown.join(", ")))
            .hint("this reviewkit does not know it: check the options or config, or update the CLI with :ReviewInstall")
            .details(serde_json::json!({ "unknown_fields": unknown })));
    }
    Ok(parsed)
}
