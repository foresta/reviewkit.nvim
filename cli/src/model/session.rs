use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{Comment, Decision, SinkResult, Target, Thread};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Session {
    pub schema_version: u32,
    pub id: String,
    pub rev: u64,
    pub state_path: PathBuf,
    pub repo: Repo,
    pub source: Source,
    pub base: RevRef,
    pub head: HeadRef,
    pub targets: Vec<Target>,
    pub comments: Vec<Comment>,
    pub decision: Option<Decision>,
    pub threads: Vec<Thread>,
    pub thread_actions: Vec<ThreadAction>,
    /// Keyed by path. A `BTreeMap` so the JSON (and therefore the etag) has a
    /// stable key order.
    pub viewed: BTreeMap<String, Viewed>,
    pub last_submit: Option<LastSubmit>,
    /// Derived from the state dir on every read rather than stored: a crash
    /// between sending and saving is exactly when the stored value is wrong.
    #[serde(default)]
    pub pending_submit: bool,
    /// A client sink's delivery waiting for the session lock to be recorded;
    /// derived like `pending_submit`.
    #[serde(default)]
    pub pending_apply: bool,
    /// Comments that left the queue, with their deliveries, so a consumed
    /// comment can still be looked up instead of being gone.
    #[serde(default)]
    pub history: Vec<Comment>,
    pub etags: Etags,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Repo {
    pub root: PathBuf,
    pub host: Option<String>,
    pub owner: Option<String>,
    pub name: Option<String>,
    pub remote_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Source {
    Pr {
        number: u64,
        url: String,
        title: String,
        author: String,
        state: PrState,
        head_ref: String,
        base_ref: String,
        base_oid: String,
        node_id: String,
        worktree: PathBuf,
    },
    Local {
        base_spec: String,
        head_spec: String,
        worktree: PathBuf,
    },
}

impl Source {
    pub fn worktree(&self) -> &Path {
        match self {
            Source::Pr { worktree, .. } | Source::Local { worktree, .. } => worktree,
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Source::Pr { .. } => "pr",
            Source::Local { .. } => "local",
        }
    }
}

/// GitHub's own spelling, so sink-github passes it through unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum PrState {
    Open,
    Closed,
    Merged,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RevRef {
    pub r#ref: Option<String>,
    pub sha: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct HeadRef {
    pub r#ref: Option<String>,
    /// `null` for a working tree: there is no commit to pin, comments anchor
    /// to blobs instead.
    pub sha: Option<String>,
    pub dirty: bool,
}

/// base / head is the vocabulary of the whole contract; GitHub's LEFT / RIGHT
/// exists only inside sink-github.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    Base,
    Head,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ThreadAction {
    pub thread_id: String,
    pub action: ThreadActionKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ThreadActionKind {
    Resolve,
    Unresolve,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Viewed {
    pub blob: String,
    pub at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct LastSubmit {
    pub sink: String,
    pub at: String,
    pub result: SinkResult,
}

/// One hash per section, so a client can tell which sections changed
/// without diffing the whole session.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Etags {
    pub targets: String,
    pub comments: String,
    pub decision: String,
    pub threads: String,
    pub thread_actions: String,
    pub viewed: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SessionSummary {
    pub id: String,
    pub kind: String,
    pub repo_root: PathBuf,
    pub title: Option<String>,
    pub comments: usize,
    pub decision: bool,
    pub updated_at: String,
}

/// A merge-base base is kept as `A...`, which already carries its dots.
fn comparison_label(base: &str, head: &str) -> String {
    if base.ends_with("...") {
        format!("{base}{head}")
    } else {
        format!("{base}..{head}")
    }
}

impl Session {
    /// `rev`, `state_path` and `etags` are placeholders here: only the store
    /// may set them, on save.
    pub fn new(id: String, repo: Repo, source: Source, base: RevRef, head: HeadRef, now: String) -> Self {
        Self {
            schema_version: super::SCHEMA_VERSION,
            id,
            rev: 0,
            state_path: PathBuf::new(),
            repo,
            source,
            base,
            head,
            targets: Vec::new(),
            comments: Vec::new(),
            decision: None,
            threads: Vec::new(),
            thread_actions: Vec::new(),
            viewed: BTreeMap::new(),
            last_submit: None,
            pending_submit: false,
            pending_apply: false,
            history: Vec::new(),
            etags: Etags::default(),
            created_at: now.clone(),
            updated_at: now,
        }
    }

    pub fn summary(&self) -> SessionSummary {
        SessionSummary {
            id: self.id.clone(),
            kind: self.source.kind().to_string(),
            repo_root: self.repo.root.clone(),
            // Local ids are hashes; the comparison is what tells them apart.
            title: match &self.source {
                Source::Pr { title, .. } => Some(title.clone()),
                Source::Local {
                    base_spec, head_spec, ..
                } => Some(comparison_label(base_spec, head_spec)),
            },
            comments: self.comments.len(),
            decision: self.decision.is_some(),
            updated_at: self.updated_at.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::{Error, ErrorCode};
    use crate::model::*;

    #[test]
    fn a_local_title_reads_as_git_range_notation() {
        assert_eq!(comparison_label("HEAD~1", "worktree"), "HEAD~1..worktree");
        assert_eq!(comparison_label("main...", "worktree"), "main...worktree");
        assert_eq!(comparison_label("main...", "feature"), "main...feature");
    }

    /// Every enum variant appears at least once, so the snapshot pins the
    /// wire spelling of the whole contract.
    fn full_session() -> Session {
        let at = "2026-01-02T03:04:05.000000Z".to_string();
        let mut session = Session::new(
            "pr/github.com/o/r/7".into(),
            Repo {
                root: PathBuf::from("/repo"),
                host: Some("github.com".into()),
                owner: Some("o".into()),
                name: Some("r".into()),
                remote_url: Some("https://github.com/o/r.git".into()),
            },
            Source::Pr {
                number: 7,
                url: "https://github.com/o/r/pull/7".into(),
                title: "Title".into(),
                author: "alice".into(),
                state: PrState::Open,
                head_ref: "feature".into(),
                base_ref: "main".into(),
                base_oid: "b".repeat(40),
                node_id: "PR_1".into(),
                worktree: PathBuf::from("/state/repos/k/pr/7"),
            },
            RevRef {
                r#ref: Some("main".into()),
                sha: "a".repeat(40),
            },
            HeadRef {
                r#ref: Some("feature".into()),
                sha: Some("c".repeat(40)),
                dirty: false,
            },
            at.clone(),
        );
        session.targets = vec![
            Target {
                id: "t_1".into(),
                path: "src/a.rs".into(),
                old_path: None,
                status: TargetStatus::Modified,
                reason: TargetReason::Changed,
                base_blob: Some("d".repeat(40)),
                head_blob: Some("e".repeat(40)),
                binary: false,
                too_large: false,
                viewed: true,
                stats: Stats { added: 2, removed: 1 },
                hunks: vec![Hunk {
                    base_start: 3,
                    base_count: 1,
                    head_start: 3,
                    head_count: 2,
                }],
                commentable: CommentableRanges {
                    base: vec![[1, 6]],
                    head: vec![[1, 7]],
                },
                range: None,
            },
            Target {
                id: "t_2".into(),
                path: "src/b.rs".into(),
                old_path: Some("src/old.rs".into()),
                status: TargetStatus::Type,
                reason: TargetReason::Thread,
                base_blob: None,
                head_blob: None,
                binary: true,
                too_large: false,
                viewed: false,
                stats: Stats::default(),
                hunks: vec![],
                commentable: CommentableRanges::default(),
                range: Some(Range { start: 1, end: 3 }),
            },
        ];
        session.comments = vec![Comment {
            id: "c_01ARZ3NDEKTSV4RRFFQ69G5FAV".into(),
            path: "src/a.rs".into(),
            side: Side::Head,
            line: 4,
            start_line: Some(3),
            kind: CommentKind::Suggestion,
            body: "use x".into(),
            suggestion: Some("let x = 1;".into()),
            reply_to: None,
            anchor: Anchor {
                base_sha: "a".repeat(40),
                head_sha: Some("c".repeat(40)),
                blob: Some("e".repeat(40)),
                context: AnchorContext {
                    before: vec!["fn f() {".into()],
                    lines: vec!["  let y = 2;".into(), "  let x = 2;".into()],
                    after: vec!["}".into()],
                },
            },
            stale: false,
            commentable: Commentable {
                github: false,
                reason: Some(Reason::OutsideDiff),
            },
            last_error: Some(LastError {
                sink: "github".into(),
                code: ErrorCode::ETool,
                message: "boom".into(),
                at: at.clone(),
            }),
            deliveries: vec![
                Delivery {
                    sink: "herdr".into(),
                    at: at.clone(),
                    mode: DeliveryMode::Staged,
                    url: None,
                    consumed: false,
                },
                Delivery {
                    sink: "clipboard".into(),
                    at: at.clone(),
                    mode: DeliveryMode::Copied,
                    url: Some("u".into()),
                    consumed: false,
                },
            ],
            created_at: at.clone(),
            updated_at: at.clone(),
        }];
        session.decision = Some(Decision {
            event: DecisionEvent::RequestChanges,
            body: "please".into(),
            updated_at: at.clone(),
            deliveries: vec![],
        });
        session.threads = vec![Thread {
            id: "PRRT_1".into(),
            path: "src/a.rs".into(),
            subject_type: SubjectType::Line,
            side: Side::Base,
            start_side: Some(Side::Base),
            line: Some(2),
            start_line: None,
            original_line: Some(2),
            resolved: false,
            outdated: true,
            comments: vec![ThreadComment {
                id: "PRRC_1".into(),
                author: "bob".into(),
                body: "why?".into(),
                created_at: at.clone(),
                url: "https://github.com/o/r/pull/7#discussion_r1".into(),
            }],
        }];
        session.thread_actions = vec![ThreadAction {
            thread_id: "PRRT_1".into(),
            action: ThreadActionKind::Resolve,
        }];
        session.viewed.insert(
            "src/a.rs".into(),
            Viewed {
                blob: "e".repeat(40),
                at: at.clone(),
            },
        );
        session.last_submit = Some(LastSubmit {
            sink: "herdr".into(),
            at: at.clone(),
            result: SinkResult {
                sink: "herdr".into(),
                at: at.clone(),
                url: None,
                sent: vec![Sent {
                    comment_id: "c_1".into(),
                    version: None,
                    remote_id: None,
                    url: None,
                }],
                failed: vec![Failed {
                    comment_id: None,
                    error: Error::new(ErrorCode::ETool, "pane gone"),
                }],
                skipped: vec![Skipped {
                    comment_id: "c_2".into(),
                    reason: Reason::NotSupported,
                }],
                thread_actions: vec![ThreadActionResult {
                    thread_id: "PRRT_1".into(),
                    action: ThreadActionKind::Unresolve,
                    status: SendStatus::Failed,
                    error: None,
                }],
                decision: DecisionStatus::NotApplicable,
                mode: DeliveryMode::Sent,
                decision_version: None,
                attempted: None,
                summary: "1 sent, 1 skipped, 1 failed".into(),
            },
        });
        session.state_path = PathBuf::from("/state/sessions/pr/github.com/o/r/7/session.json");
        session
    }

    #[test]
    fn wire_format_matches_the_contract() {
        insta::assert_json_snapshot!(full_session());
    }

    #[test]
    fn wire_format_roundtrips() {
        let json = serde_json::to_value(full_session()).unwrap();
        let back: Session = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(serde_json::to_value(back).unwrap(), json);
    }

    #[test]
    fn local_source_is_tagged_by_kind() {
        let json = serde_json::to_value(Source::Local {
            base_spec: "HEAD~1".into(),
            head_spec: "worktree".into(),
            worktree: PathBuf::from("/repo"),
        })
        .unwrap();
        assert_eq!(json["kind"], "local");
    }
}
