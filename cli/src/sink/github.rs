use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::core::time;
use crate::error::{Error, ErrorCode, Result};
use crate::ext::gh::{self, Gh, RepoRef};
use crate::model::DeliveryMode;
use crate::model::session::ThreadActionKind;
use crate::model::{
    Comment, DecisionEvent, DecisionStatus, Failed, Reason, SendStatus, Sent, Session, Side, SinkResult, Skipped,
    Source, ThreadActionResult,
};

pub const NAME: &str = "github";

#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Plan {
    pub mutation: String,
    #[schemars(with = "serde_json::Map<String, serde_json::Value>")]
    pub variables: serde_json::Value,
    pub skipped: Vec<Skipped>,
    /// Comment ids in the order of `variables.threads`.
    pub thread_comment_ids: Vec<String>,
    pub replies: Vec<PlannedReply>,
    pub actions: Vec<PlannedAction>,
    pub event: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PlannedReply {
    pub alias: String,
    pub comment_id: String,
    pub thread_id: String,
    pub body: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PlannedAction {
    pub alias: String,
    pub thread_id: String,
    pub action: ThreadActionKind,
}

/// Marked in `details` so a caller can tell "the queue is empty" from
/// other invalid states without matching the message.
fn nothing_to_submit() -> Error {
    Error::invalid("nothing to submit: no sendable comments, decision or thread actions")
        .details(serde_json::json!({ "empty": true }))
}

pub fn is_nothing_to_submit(err: &Error) -> bool {
    err.code == ErrorCode::EInvalid && err.details["empty"] == true
}

/// base/head becomes LEFT/RIGHT only here (design §3).
fn side_name(side: Side) -> &'static str {
    match side {
        Side::Base => "LEFT",
        Side::Head => "RIGHT",
    }
}

/// The decision becomes a PullRequestReviewEvent only here.
fn event_name(event: DecisionEvent) -> &'static str {
    match event {
        DecisionEvent::Approve => "APPROVE",
        DecisionEvent::RequestChanges => "REQUEST_CHANGES",
        DecisionEvent::Comment => "COMMENT",
    }
}

/// GitHub renders a suggestion only from a fenced block of that language.
fn body_with_suggestion(comment: &Comment) -> String {
    // An empty suggestion is GitHub's way to propose deleting the lines,
    // so it still gets its (empty) fence.
    match comment.suggestion.as_deref() {
        Some(suggestion) if suggestion.trim().is_empty() => {
            format!("{}\n\n```suggestion\n```", comment.body.trim_end())
        }
        Some(suggestion) => format!(
            "{}\n\n```suggestion\n{}\n```",
            comment.body.trim_end(),
            suggestion.trim_end()
        ),
        None => comment.body.clone(),
    }
}

fn skip_reason(comment: &Comment) -> Option<Reason> {
    if comment.stale {
        return Some(Reason::Stale);
    }
    if !comment.commentable.github {
        return Some(comment.commentable.reason.unwrap_or(Reason::OutsideDiff));
    }
    None
}

/// One request for everything (architecture D1): the review with its
/// inline threads, thread replies and resolve/unresolve as aliased fields of
/// a single mutation, so a partial failure is reported per alias.
pub fn plan(session: &Session) -> Result<Plan> {
    let Source::Pr { node_id, .. } = &session.source else {
        return Err(Error::invalid("the github sink needs a pull request session"));
    };
    let head = session
        .head
        .sha
        .as_deref()
        .ok_or_else(|| Error::invalid("the session has no head commit to review"))?;

    let mut skipped = Vec::new();
    let mut threads = Vec::new();
    let mut thread_comment_ids = Vec::new();
    let mut replies = Vec::new();
    for comment in &session.comments {
        // A reply is posted into its thread, not at a line, so a moved or
        // outdated line is no reason to hold it back.
        if let Some(reason) = skip_reason(comment).filter(|_| comment.reply_to.is_none()) {
            skipped.push(Skipped {
                comment_id: comment.id.clone(),
                reason,
            });
            continue;
        }
        if let Some(thread_id) = &comment.reply_to {
            replies.push(PlannedReply {
                alias: format!("reply{}", replies.len()),
                comment_id: comment.id.clone(),
                thread_id: thread_id.clone(),
                body: body_with_suggestion(comment),
            });
            continue;
        }
        let mut thread = serde_json::json!({
            "path": comment.path,
            "line": comment.line,
            "side": side_name(comment.side),
            "body": body_with_suggestion(comment),
        });
        if let Some(start) = comment.start_line.filter(|s| *s < comment.line) {
            thread["startLine"] = serde_json::json!(start);
            thread["startSide"] = serde_json::json!(side_name(comment.side));
        }
        threads.push(thread);
        thread_comment_ids.push(comment.id.clone());
    }

    // A decision without a body still needs the review; inline comments
    // without a decision go as a COMMENT review with an empty body.
    let event = match (&session.decision, threads.is_empty()) {
        (Some(decision), _) => Some(event_name(decision.event).to_string()),
        (None, false) => Some("COMMENT".to_string()),
        (None, true) => None,
    };
    // GitHub rejects COMMENT and REQUEST_CHANGES with neither a body nor
    // inline threads; caught here so it is a pre-send error, not a failed send.
    if let Some(decision) = &session.decision
        && threads.is_empty()
        && decision.body.trim().is_empty()
        && decision.event != DecisionEvent::Approve
    {
        return Err(Error::invalid(format!(
            "GitHub needs a body or inline comments for {}",
            event_name(decision.event)
        ))
        .hint("write the decision body"));
    }
    if event.is_none() && replies.is_empty() && session.thread_actions.is_empty() {
        return Err(nothing_to_submit());
    }

    let mut declarations: Vec<String> = Vec::new();
    let mut fields = Vec::new();
    let mut variables = serde_json::Map::new();
    if let Some(event) = &event {
        declarations.extend(
            [
                "$prId: ID!",
                "$commit: GitObjectID",
                "$event: PullRequestReviewEvent!",
                "$body: String",
                "$threads: [DraftPullRequestReviewThread]",
            ]
            .map(String::from),
        );
        fields.push(
            "review: addPullRequestReview(input: {pullRequestId: $prId, commitOID: $commit, event: $event, body: $body, threads: $threads}) { pullRequestReview { id url } }"
                .to_string(),
        );
        variables.insert("prId".into(), serde_json::json!(node_id));
        variables.insert("commit".into(), serde_json::json!(head));
        variables.insert("event".into(), serde_json::json!(event));
        variables.insert(
            "body".into(),
            serde_json::json!(session.decision.as_ref().map(|d| d.body.clone()).unwrap_or_default()),
        );
        variables.insert("threads".into(), serde_json::Value::Array(threads));
    }
    for reply in &replies {
        let alias = &reply.alias;
        declarations.push(format!("${alias}: AddPullRequestReviewThreadReplyInput!"));
        fields.push(format!(
            "{alias}: addPullRequestReviewThreadReply(input: ${alias}) {{ comment {{ id url }} }}"
        ));
        variables.insert(
            alias.clone(),
            serde_json::json!({ "pullRequestReviewThreadId": reply.thread_id, "body": reply.body }),
        );
    }
    let mut actions = Vec::new();
    for (i, action) in session.thread_actions.iter().enumerate() {
        let alias = format!("action{i}");
        let name = match action.action {
            ThreadActionKind::Resolve => "resolveReviewThread",
            ThreadActionKind::Unresolve => "unresolveReviewThread",
        };
        declarations.push(format!("${alias}: ID!"));
        fields.push(format!(
            "{alias}: {name}(input: {{threadId: ${alias}}}) {{ thread {{ id isResolved }} }}"
        ));
        variables.insert(alias.clone(), serde_json::json!(action.thread_id));
        actions.push(PlannedAction {
            alias,
            thread_id: action.thread_id.clone(),
            action: action.action,
        });
    }
    let mutation = format!("mutation({}) {{ {} }}", declarations.join(", "), fields.join(" "));
    Ok(Plan {
        mutation,
        variables: serde_json::Value::Object(variables),
        skipped,
        thread_comment_ids,
        replies,
        actions,
        event,
    })
}

/// Written before gh is called and removed only after the answer was
/// applied and saved, so a crash anywhere in between leaves a record of
/// what may already be on GitHub. An earlier record is always reconciled
/// and saved before a new one is written over it.
#[derive(Debug, Serialize, Deserialize)]
pub struct PendingSubmit {
    pub at: String,
    pub comments: Vec<PendingComment>,
    #[serde(default)]
    pub replies: Vec<PendingReply>,
    /// The PullRequestReviewEvent sent with this attempt, if any.
    #[serde(default)]
    pub event: Option<String>,
    #[serde(default)]
    pub decision_body: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PendingComment {
    pub id: String,
    pub path: String,
    pub side: Side,
    pub line: u32,
    pub body: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PendingReply {
    pub id: String,
    pub thread_id: String,
    pub body: String,
}

pub fn pending_path(session_dir: &Path) -> std::path::PathBuf {
    session_dir.join(crate::store::PENDING_SUBMIT_FILE)
}

/// The same text as GitHub stores it: line endings normalised, outer
/// whitespace ignored.
fn same_text(a: &str, b: &str) -> bool {
    a.replace("\r\n", "\n").trim() == b.replace("\r\n", "\n").trim()
}

/// GitHub's review states for each event we send.
fn review_state(event: &str) -> &'static str {
    match event {
        "APPROVE" => "APPROVED",
        "REQUEST_CHANGES" => "CHANGES_REQUESTED",
        _ => "COMMENTED",
    }
}

/// GitHub reports `submittedAt` in whole seconds and its clock is not ours,
/// so "after the attempt began" allows a few minutes of slack.
const CLOCK_SLACK_SECONDS: i64 = 300;

fn after(submitted: &str, attempt: &str) -> bool {
    match (submitted.parse::<jiff::Timestamp>(), attempt.parse::<jiff::Timestamp>()) {
        (Ok(submitted), Ok(attempt)) => submitted.as_second() >= attempt.as_second() - CLOCK_SLACK_SECONDS,
        _ => false,
    }
}

/// A previous submit that never got its answer applied: what already
/// appears on GitHub as this user's post is reported as sent, so nothing
/// is posted twice. Something edited since the attempt no longer equals
/// what was posted and stays queued. The file is left in place; the caller
/// removes it once the result is saved.
pub fn reconcile_pending(
    session: &mut Session,
    gh: &dyn Gh,
    repo: &RepoRef,
    session_dir: &Path,
) -> Result<Option<SinkResult>> {
    let path = pending_path(session_dir);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err.into()),
    };
    let pending: PendingSubmit = serde_json::from_slice(&bytes).map_err(|err| {
        Error::internal(format!("cannot read {}: {err}", path.display()))
            .hint("delete the file after checking the pull request on GitHub")
    })?;
    let Source::Pr { number, .. } = &session.source else {
        return Ok(None);
    };
    let details = gh::pr_details(gh, repo, *number, time::now())?;
    let viewer = details.viewer.as_str();
    let mine = |author: &str| viewer.is_empty() || author == viewer;
    let unchanged = |id: &str, body: &str| {
        session
            .comments
            .iter()
            .any(|c| c.id == id && same_text(&body_with_suggestion(c), body))
    };

    let mut sent = Vec::new();
    // Each remote post vouches for one pending comment only: two queued
    // comments with the same text must not both count as posted when only
    // one reached GitHub.
    let mut claimed: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let mut claim = |thread: &dyn Fn(&crate::model::Thread) -> bool, body: &str| -> bool {
        for t in details.threads.iter().filter(|t| thread(t)) {
            for c in &t.comments {
                if !claimed.contains(c.id.as_str())
                    && mine(&c.author)
                    && same_text(&c.body, body)
                    && after(&c.created_at, &pending.at)
                {
                    claimed.insert(c.id.as_str());
                    return true;
                }
            }
        }
        false
    };
    // Not matched by line: a push between the attempt and now moves or
    // outdates the thread, and a missed match posts the comment twice.
    for p in &pending.comments {
        let posted = claim(&|t| t.path == p.path && t.side == p.side, &p.body);
        if posted && unchanged(&p.id, &p.body) {
            sent.push(Sent {
                comment_id: p.id.clone(),
                version: None,
                remote_id: None,
                url: None,
            });
        }
    }
    for r in &pending.replies {
        let posted = claim(&|t| t.id == r.thread_id, &r.body);
        if posted && unchanged(&r.id, &r.body) {
            sent.push(Sent {
                comment_id: r.id.clone(),
                version: None,
                remote_id: None,
                url: None,
            });
        }
    }
    let decision_unchanged = session.decision.as_ref().is_some_and(|d| {
        Some(event_name(d.event)) == pending.event.as_deref() && same_text(&d.body, &pending.decision_body)
    });
    let decision_posted = decision_unchanged
        && pending.event.as_deref().is_some_and(|event| {
            details.info.timeline.iter().any(|item| {
                item.kind == crate::model::TimelineKind::Review
                    && mine(&item.author)
                    && item.state.as_deref() == Some(review_state(event))
                    && same_text(item.body.as_deref().unwrap_or(""), &pending.decision_body)
                    && after(&item.created_at, &pending.at)
            })
        });
    session.threads = details.threads;
    crate::core::threads::prune_actions(session);
    if sent.is_empty() && !decision_posted {
        return Ok(None);
    }
    Ok(Some(SinkResult {
        sink: NAME.into(),
        at: String::new(),
        url: None,
        sent,
        failed: Vec::new(),
        skipped: Vec::new(),
        thread_actions: Vec::new(),
        decision: if decision_posted {
            DecisionStatus::Sent
        } else {
            DecisionStatus::NotApplicable
        },
        summary: String::new(),
        mode: DeliveryMode::Sent,
        attempted: None,
    }))
}

fn write_pending(session: &Session, plan: &Plan, session_dir: &Path) -> Result<()> {
    let comments = session
        .comments
        .iter()
        .filter(|c| plan.thread_comment_ids.contains(&c.id))
        .map(|c| PendingComment {
            id: c.id.clone(),
            path: c.path.clone(),
            side: c.side,
            line: c.line,
            body: body_with_suggestion(c),
        })
        .collect();
    let replies = plan
        .replies
        .iter()
        .map(|r| PendingReply {
            id: r.comment_id.clone(),
            thread_id: r.thread_id.clone(),
            body: r.body.clone(),
        })
        .collect();
    let pending = PendingSubmit {
        at: time::now(),
        comments,
        replies,
        event: plan.event.clone(),
        decision_body: session.decision.as_ref().map(|d| d.body.clone()).unwrap_or_default(),
    };
    let bytes = serde_json::to_vec_pretty(&pending)
        .map_err(|err| Error::internal(format!("cannot serialize pending_submit.json: {err}")))?;
    // Atomic, so a crash mid-write cannot leave a file that silently
    // disables the duplicate check.
    let mut tmp = tempfile::NamedTempFile::new_in(session_dir)?;
    std::io::Write::write_all(&mut tmp, &bytes)?;
    tmp.persist(pending_path(session_dir))
        .map_err(|err| Error::internal(format!("cannot write pending_submit.json: {}", err.error)))?;
    Ok(())
}

/// Sends the plan. Everything before gh is called is a hard error; once the
/// mutation was sent, the answer is turned into a SinkResult per alias so
/// the queue keeps exactly what did not get through. `pending_submit.json`
/// is left for the caller to remove after the result is saved.
pub fn submit(session: &Session, gh: &dyn Gh, repo: &RepoRef, session_dir: &Path) -> Result<SinkResult> {
    let plan = plan(session)?;
    write_pending(session, &plan, session_dir)?;
    let response = gh::graphql(gh, &repo.host, &plan.mutation, plan.variables.clone())?;
    let at = time::now();
    let mut sent = Vec::new();
    let mut failed = Vec::new();
    let mut thread_actions = Vec::new();
    let mut decision = DecisionStatus::NotApplicable;
    let mut url = None;

    // Errors name the failed field through `path[0]`; one without a path is
    // the whole request failing.
    let error_for = |alias: &str| -> Option<Error> {
        response
            .errors
            .iter()
            .find(|e| e["path"][0].as_str() == Some(alias) || e["path"].is_null())
            .and_then(|e| e["message"].as_str())
            .map(gh::graphql_error)
    };

    if plan.event.is_some() {
        let review = &response.data["review"]["pullRequestReview"];
        if review.is_object() {
            url = review["url"].as_str().map(str::to_string);
            let review_id = review["id"].as_str().map(str::to_string);
            for id in &plan.thread_comment_ids {
                sent.push(Sent {
                    comment_id: id.clone(),
                    version: None,
                    remote_id: review_id.clone(),
                    url: url.clone(),
                });
            }
            if session.decision.is_some() {
                decision = DecisionStatus::Sent;
            }
        } else {
            let error = error_for("review").unwrap_or_else(|| Error::new(ErrorCode::EGh, "the review was not created"));
            if session.decision.is_some() {
                decision = DecisionStatus::Failed;
            }
            // Per comment, so each keeps its own last_error even when a reply
            // in the same request succeeded.
            for id in &plan.thread_comment_ids {
                failed.push(Failed {
                    comment_id: Some(id.clone()),
                    error: error.clone(),
                });
            }
            if plan.thread_comment_ids.is_empty() {
                failed.push(Failed {
                    comment_id: None,
                    error,
                });
            }
        }
    }
    for reply in &plan.replies {
        let node = &response.data[&reply.alias]["comment"];
        if node.is_object() {
            sent.push(Sent {
                comment_id: reply.comment_id.clone(),
                version: None,
                remote_id: node["id"].as_str().map(str::to_string),
                url: node["url"].as_str().map(str::to_string),
            });
        } else {
            failed.push(Failed {
                comment_id: Some(reply.comment_id.clone()),
                error: error_for(&reply.alias)
                    .unwrap_or_else(|| Error::new(ErrorCode::EGh, "the reply was not created")),
            });
        }
    }
    for action in &plan.actions {
        let ok = response.data[&action.alias]["thread"].is_object();
        thread_actions.push(ThreadActionResult {
            thread_id: action.thread_id.clone(),
            action: action.action,
            status: if ok { SendStatus::Sent } else { SendStatus::Failed },
            error: if ok {
                None
            } else {
                Some(
                    error_for(&action.alias)
                        .unwrap_or_else(|| Error::new(ErrorCode::EGh, "the thread was not updated")),
                )
            },
        });
    }
    Ok(SinkResult {
        sink: NAME.into(),
        at,
        url,
        sent,
        failed,
        skipped: plan.skipped,
        thread_actions,
        decision,
        summary: String::new(),
        mode: DeliveryMode::Sent,
        attempted: Some(
            plan.thread_comment_ids
                .iter()
                .cloned()
                .chain(plan.replies.iter().map(|r| r.comment_id.clone()))
                .collect(),
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        Anchor, AnchorContext, CommentKind, Commentable, Decision, HeadRef, PrState, Repo, RevRef, ThreadAction,
    };
    use std::path::PathBuf;

    fn comment(id: &str, line: u32, start: Option<u32>, side: Side) -> Comment {
        Comment {
            id: id.into(),
            path: "src/a.rs".into(),
            side,
            line,
            start_line: start,
            kind: CommentKind::Issue,
            body: format!("body of {id}"),
            suggestion: None,
            reply_to: None,
            anchor: Anchor {
                base_sha: "a".repeat(40),
                head_sha: Some("c".repeat(40)),
                blob: None,
                context: AnchorContext::default(),
            },
            stale: false,
            commentable: Commentable {
                github: true,
                reason: None,
            },
            last_error: None,
            deliveries: Vec::new(),
            created_at: "t".into(),
            updated_at: "t".into(),
        }
    }

    fn pr_session() -> Session {
        let mut s = Session::new(
            "pr/github.com/o/r/7".into(),
            Repo {
                root: PathBuf::from("/repo"),
                host: Some("github.com".into()),
                owner: Some("o".into()),
                name: Some("r".into()),
                remote_url: None,
            },
            Source::Pr {
                number: 7,
                url: "https://github.com/o/r/pull/7".into(),
                title: "T".into(),
                author: "a".into(),
                state: PrState::Open,
                head_ref: "f".into(),
                base_ref: "main".into(),
                base_oid: "b".repeat(40),
                node_id: "PR_node".into(),
                worktree: PathBuf::from("/wt"),
            },
            RevRef {
                r#ref: Some("main".into()),
                sha: "a".repeat(40),
            },
            HeadRef {
                r#ref: Some("f".into()),
                sha: Some("c".repeat(40)),
                dirty: false,
            },
            "t".into(),
        );
        s.comments = vec![
            comment("c_1", 3, None, Side::Head),
            comment("c_2", 10, Some(8), Side::Base),
            comment("c_3", 5, None, Side::Head),
            comment("c_4", 6, None, Side::Head),
            comment("c_5", 7, None, Side::Head),
        ];
        s.comments[1].suggestion = Some("let x = 1;".into());
        s.comments[2].stale = true;
        s.comments[3].commentable = Commentable {
            github: false,
            reason: Some(Reason::OutsideDiff),
        };
        s.comments[4].reply_to = Some("PRRT_1".into());
        s.decision = Some(Decision {
            event: DecisionEvent::RequestChanges,
            body: "please".into(),
            updated_at: "t".into(),
        });
        s.thread_actions = vec![ThreadAction {
            thread_id: "PRRT_2".into(),
            action: ThreadActionKind::Resolve,
        }];
        s
    }

    #[test]
    fn plan_variables_match_the_github_api() {
        let plan = plan(&pr_session()).unwrap();
        insta::assert_json_snapshot!(plan.variables);
        insta::assert_snapshot!(plan.mutation);
        assert_eq!(plan.thread_comment_ids, vec!["c_1", "c_2"]);
        let skipped: Vec<(&str, Reason)> = plan.skipped.iter().map(|s| (s.comment_id.as_str(), s.reason)).collect();
        assert_eq!(skipped, vec![("c_3", Reason::Stale), ("c_4", Reason::OutsideDiff)]);
        assert_eq!(plan.replies.len(), 1);
        assert_eq!(
            (plan.replies[0].alias.as_str(), plan.replies[0].comment_id.as_str()),
            ("reply0", "c_5")
        );
    }

    #[test]
    fn an_empty_suggestion_is_sent_as_an_empty_fence() {
        let mut s = pr_session();
        s.comments[0].suggestion = Some(String::new());
        assert!(body_with_suggestion(&s.comments[0]).ends_with("```suggestion\n```"));
        s.comments[0].suggestion = None;
        assert!(!body_with_suggestion(&s.comments[0]).contains("```"));
    }

    #[test]
    fn a_reply_whose_line_moved_or_left_the_diff_is_still_sent() {
        let mut s = pr_session();
        s.comments[4].stale = true;
        s.comments[4].commentable = Commentable {
            github: false,
            reason: Some(Reason::OutsideDiff),
        };
        let plan = plan(&s).unwrap();
        assert_eq!(plan.replies.len(), 1);
        assert!(plan.skipped.iter().all(|sk| sk.comment_id != "c_5"));
    }

    #[test]
    fn comments_without_a_decision_become_a_comment_review_with_an_empty_body() {
        let mut s = pr_session();
        s.decision = None;
        let plan = plan(&s).unwrap();
        assert_eq!(plan.event.as_deref(), Some("COMMENT"));
        assert_eq!(plan.variables["body"], "");
    }

    #[test]
    fn a_bodiless_request_changes_without_threads_is_rejected_before_sending() {
        let mut s = pr_session();
        s.comments.clear();
        s.thread_actions.clear();
        s.decision.as_mut().unwrap().body = "  ".into();
        assert_eq!(plan(&s).unwrap_err().code, ErrorCode::EInvalid);
        s.decision.as_mut().unwrap().event = DecisionEvent::Approve;
        assert_eq!(plan(&s).unwrap().event.as_deref(), Some("APPROVE"));
    }

    #[test]
    fn nothing_sendable_is_invalid_and_local_sessions_are_rejected() {
        let mut s = pr_session();
        s.comments.clear();
        s.decision = None;
        s.thread_actions.clear();
        assert_eq!(plan(&s).unwrap_err().code, ErrorCode::EInvalid);
        s.source = Source::Local {
            base_spec: "HEAD".into(),
            head_spec: "worktree".into(),
            worktree: PathBuf::from("/repo"),
        };
        assert_eq!(plan(&s).unwrap_err().code, ErrorCode::EInvalid);
    }
}
