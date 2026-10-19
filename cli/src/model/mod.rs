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
    AgentType, DecisionStatus, Failed, SendStatus, Sent, SinkResult, Skipped, SubmitOptions, ThreadActionResult,
};
pub use target::{CommentableRanges, Range, Stats, Target, TargetReason, TargetStatus};
pub use thread::{SubjectType, Thread, ThreadComment};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Bumped only for incompatible changes. Within one version, fields and enum
/// values are only ever added.
pub const SCHEMA_VERSION: u32 = 1;

/// Bumped for every field or enum value added to a persisted type. A CLI
/// does not write a session last written by a higher revision: it would drop
/// what it cannot represent. Recorded outside the contract, in `_writer`.
pub const SCHEMA_REVISION: u32 = 3;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct VersionInfo {
    pub version: String,
    pub schema_version: u32,
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
    pub sink_result: SinkResult,
    pub sink_info: crate::sink::SinkInfo,
    pub herdr_preview: crate::sink::herdr::Preview,
    pub github_plan: crate::sink::github::Plan,
    pub herdr_candidate: crate::sink::herdr::Candidate,
    pub herdr_agent: crate::ext::herdr::Agent,
    pub herdr_pane: crate::ext::herdr::Pane,
}
