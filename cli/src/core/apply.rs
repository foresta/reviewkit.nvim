use crate::core::time;
use crate::error::Result;
use crate::model::{
    DecisionStatus, Delivery, DeliveryMode, LastError, LastSubmit, Reason, SendStatus, Session, SinkResult,
};

/// Applies a sink's report to the queue (architecture D4). `consume` is the
/// sink's call (`Sink::consumes`); staged or copied text never consumes.
pub fn apply(session: &mut Session, mut result: SinkResult, consume: bool) -> Result<SinkResult> {
    if result.at.is_empty() {
        result.at = time::now();
    }
    if result.summary.is_empty() {
        result.summary = summary(&result);
    }
    let consume = consume && result.mode == DeliveryMode::Sent;
    // A client sink reporting a comment it was not given cannot deliver it.
    if let Some(attempted) = &result.attempted {
        let reported = result.sent.len();
        result.sent.retain(|s| attempted.contains(&s.comment_id));
        if result.sent.len() != reported {
            result.summary = summary(&result);
        }
    }
    let edited_meanwhile = |comment: &crate::model::Comment, sent: &crate::model::Sent| {
        sent.version.as_deref().is_some_and(|v| v != comment.updated_at)
    };
    // An edit while the sink was sending means the delivered text is not
    // the comment's text any more: it is recorded as not sent, wherever the
    // comment is now. Entries for comments in neither the queue nor the
    // history are kept as reported.
    let edited: Vec<String> = result
        .sent
        .iter()
        .filter(|s| {
            session
                .comments
                .iter()
                .chain(session.history.iter())
                .find(|c| c.id == s.comment_id)
                .is_some_and(|c| edited_meanwhile(c, s))
        })
        .map(|s| s.comment_id.clone())
        .collect();
    let reported = result.sent.len();
    result.sent.retain(|s| !edited.contains(&s.comment_id));
    let delivered: Vec<String> = result.sent.iter().map(|s| s.comment_id.clone()).collect();
    if result.sent.len() != reported {
        result.summary = summary(&result);
    }
    for sent in &result.sent {
        // A comment that already left the queue (another sink consumed it)
        // still gets the delivery, so the history knows where it went.
        let found = session
            .comments
            .iter_mut()
            .chain(session.history.iter_mut())
            .find(|c| c.id == sent.comment_id);
        if let Some(comment) = found {
            comment.last_error = None;
            if !recorded(&comment.deliveries, &result) {
                comment.deliveries.push(Delivery {
                    sink: result.sink.clone(),
                    at: result.at.clone(),
                    mode: result.mode,
                    url: sent.url.clone(),
                    consumed: consume,
                });
            }
        }
    }
    if consume {
        let (gone, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut session.comments)
            .into_iter()
            .partition(|c| delivered.contains(&c.id));
        session.comments = kept;
        retire(&mut session.history, gone);
    }
    for failed in &result.failed {
        let Some(comment_id) = &failed.comment_id else { continue };
        if let Some(comment) = session.comments.iter_mut().find(|c| &c.id == comment_id) {
            comment.last_error = Some(LastError {
                sink: result.sink.clone(),
                code: failed.error.code,
                message: failed.error.message.clone(),
                at: result.at.clone(),
            });
        }
    }
    // A whole-review failure (comment_id null) marks every comment that was
    // part of the attempt, so the client can show why nothing left the queue.
    // Skipped comments, and comments this sink already delivered (it does not
    // send them again), were never part of it.
    if result.sent.is_empty()
        && let Some(whole) = result.failed.iter().find(|f| f.comment_id.is_none())
    {
        let error = whole.error.clone();
        let sink = result.sink.clone();
        let attempted = result.attempted.clone();
        for comment in session.comments.iter_mut().filter(|c| {
            attempted.as_ref().is_none_or(|ids| ids.contains(&c.id))
                && !result.skipped.iter().any(|s| s.comment_id == c.id)
                && !c
                    .deliveries
                    .iter()
                    .any(|d| d.sink == sink && d.mode == DeliveryMode::Sent)
        }) {
            comment.last_error = Some(LastError {
                sink: result.sink.clone(),
                code: error.code,
                message: error.message.clone(),
                at: result.at.clone(),
            });
        }
    }
    session.thread_actions.retain(|action| {
        !result
            .thread_actions
            .iter()
            .any(|r| r.thread_id == action.thread_id && r.status == SendStatus::Sent)
    });
    let same_decision = result
        .decision_version
        .as_deref()
        .is_none_or(|v| session.decision.as_ref().is_some_and(|d| d.updated_at == v));
    if result.decision == DecisionStatus::Sent && same_decision {
        if let Some(decision) = session.decision.as_mut().filter(|d| !recorded(&d.deliveries, &result)) {
            decision.deliveries.push(Delivery {
                sink: result.sink.clone(),
                at: result.at.clone(),
                mode: result.mode,
                url: result.url.clone(),
                consumed: consume,
            });
        }
        if consume {
            session.decision = None;
        }
    }
    session.last_submit = Some(LastSubmit {
        sink: result.sink.clone(),
        at: result.at.clone(),
        result: result.clone(),
    });
    Ok(result)
}

/// A queued result replayed after a crash between saving and removing it.
fn recorded(deliveries: &[Delivery], result: &SinkResult) -> bool {
    deliveries
        .iter()
        .any(|d| d.sink == result.sink && d.at == result.at && d.mode == result.mode)
}

/// A local session is reused for every `:Review diff` of the same specs,
/// so without a limit the history only grows.
pub const HISTORY_LIMIT: usize = 200;

/// Comments only cleared from the queue are kept up to this many: they were
/// sent nowhere final and would be lost for good, but kept without limit
/// they grow the session file on every clear.
pub const CLEARED_LIMIT: usize = 1000;

/// Oldest first. Comments that reached their destination go first to stay
/// within the limit; cleared ones only past `CLEARED_LIMIT`.
pub fn retire(history: &mut Vec<crate::model::Comment>, gone: Vec<crate::model::Comment>) {
    history.extend(gone);
    let mut excess = history.len().saturating_sub(HISTORY_LIMIT);
    history.retain(|c| {
        if excess > 0 && c.deliveries.iter().any(|d| d.consumed) {
            excess -= 1;
            return false;
        }
        true
    });
    let over = history.len().saturating_sub(CLEARED_LIMIT);
    history.drain(..over);
}

/// "3 sent, 1 skipped (outside diff), 0 failed"
pub fn summary(result: &SinkResult) -> String {
    // A copy or a staged prompt reached nobody yet; "sent" would say it did.
    let verb = match result.mode {
        DeliveryMode::Sent => "sent",
        DeliveryMode::Staged => "staged",
        DeliveryMode::Copied => "copied",
    };
    let mut parts = vec![format!("{} {verb}", result.sent.len())];
    if !result.skipped.is_empty() {
        let mut reasons: Vec<&str> = result.skipped.iter().map(|s| reason_label(s.reason)).collect();
        reasons.sort_unstable();
        reasons.dedup();
        parts.push(format!("{} skipped ({})", result.skipped.len(), reasons.join(", ")));
    }
    parts.push(format!("{} failed", result.failed.len()));
    match result.decision {
        DecisionStatus::Sent => parts.push(format!("decision {verb}")),
        DecisionStatus::Failed => parts.push("decision failed".into()),
        DecisionStatus::Skipped => parts.push("decision skipped".into()),
        DecisionStatus::NotApplicable => {}
    }
    parts.join(", ")
}

fn reason_label(reason: Reason) -> &'static str {
    match reason {
        Reason::OutsideDiff => "outside diff",
        Reason::Stale => "stale",
        Reason::Binary => "binary",
        Reason::NotSupported => "not supported",
        Reason::Delivered => "already delivered",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::{Error, ErrorCode};
    use crate::model::{Failed, Sent, Skipped};

    fn result(sent: &[&str], failed: &[Option<&str>], skipped: &[&str], decision: DecisionStatus) -> SinkResult {
        SinkResult {
            sink: "test".into(),
            at: String::new(),
            url: None,
            sent: sent
                .iter()
                .map(|id| Sent {
                    comment_id: (*id).into(),
                    version: None,
                    remote_id: None,
                    url: None,
                })
                .collect(),
            failed: failed
                .iter()
                .map(|id| Failed {
                    comment_id: id.map(str::to_string),
                    error: Error::new(ErrorCode::ETool, "boom"),
                })
                .collect(),
            skipped: skipped
                .iter()
                .map(|id| Skipped {
                    comment_id: (*id).into(),
                    reason: Reason::OutsideDiff,
                })
                .collect(),
            thread_actions: vec![],
            decision,
            mode: DeliveryMode::Sent,
            decision_version: None,
            attempted: None,
            summary: String::new(),
        }
    }

    #[test]
    fn the_history_drops_only_the_oldest_consumed_comments() {
        let comment = |n: usize, consumed: bool| {
            serde_json::from_value::<crate::model::Comment>(serde_json::json!({
                "id": format!("c_{n}"), "path": "a", "side": "head", "line": 1, "kind": "issue",
                "body": "b", "anchor": { "base_sha": "a", "head_sha": null, "blob": null,
                    "context": { "before": [], "lines": [], "after": [] } },
                "stale": false, "commentable": { "github": true, "reason": null },
                "deliveries": [{ "sink": "x", "at": "t", "mode": "sent", "consumed": consumed }],
                "created_at": "t", "updated_at": "t"
            }))
            .unwrap()
        };
        let mut history = vec![comment(0, false)];
        history.extend((1..HISTORY_LIMIT).map(|n| comment(n, true)));
        retire(
            &mut history,
            vec![comment(HISTORY_LIMIT, true), comment(HISTORY_LIMIT + 1, true)],
        );
        assert_eq!(history.len(), HISTORY_LIMIT);
        assert_eq!(
            history[0].id, "c_0",
            "a comment only cleared never reached its destination"
        );
        assert_eq!(history[1].id, "c_3", "the oldest consumed ones go");
        let mut cleared: Vec<_> = (0..HISTORY_LIMIT + 5).map(|n| comment(n, false)).collect();
        retire(&mut cleared, vec![]);
        assert_eq!(
            cleared.len(),
            HISTORY_LIMIT + 5,
            "cleared ones stay past the history limit"
        );
        let mut many: Vec<_> = (0..CLEARED_LIMIT + 3).map(|n| comment(n, false)).collect();
        retire(&mut many, vec![]);
        assert_eq!(many.len(), CLEARED_LIMIT);
        assert_eq!(many[0].id, "c_3", "the oldest cleared ones go past their own limit");
    }

    #[test]
    fn a_replayed_result_records_no_second_delivery() {
        use crate::model::{HeadRef, Repo, RevRef, Source};
        use std::path::PathBuf;
        let mut session = Session::new(
            "local/x/HEAD..worktree".into(),
            Repo {
                root: PathBuf::from("/repo"),
                host: None,
                owner: None,
                name: None,
                remote_url: None,
            },
            Source::Local {
                base_spec: "HEAD".into(),
                head_spec: "worktree".into(),
                worktree: PathBuf::from("/repo"),
            },
            RevRef {
                r#ref: Some("HEAD".into()),
                sha: "a".repeat(40),
            },
            HeadRef {
                r#ref: None,
                sha: None,
                dirty: false,
            },
            "t".into(),
        );
        session.comments.push(
            serde_json::from_value(serde_json::json!({
                "id": "c_1", "path": "a", "side": "head", "line": 1, "kind": "issue",
                "body": "b", "anchor": { "base_sha": "a", "head_sha": null, "blob": null,
                    "context": { "before": [], "lines": [], "after": [] } },
                "stale": false, "commentable": { "github": true, "reason": null },
                "created_at": "t", "updated_at": "t"
            }))
            .unwrap(),
        );
        session.decision = Some(
            serde_json::from_value(serde_json::json!({ "event": "comment", "body": "b", "updated_at": "t" })).unwrap(),
        );
        let mut once = result(&["c_1"], &[], &[], DecisionStatus::Sent);
        once.at = "2026-01-01T00:00:00.000001Z".into();
        once.mode = DeliveryMode::Copied;
        apply(&mut session, once.clone(), false).unwrap();
        apply(&mut session, once, false).unwrap();
        assert_eq!(session.comments[0].deliveries.len(), 1);
        assert_eq!(session.decision.as_ref().unwrap().deliveries.len(), 1);
    }

    #[test]
    fn an_old_version_delivered_to_a_comment_now_in_the_history_records_nothing() {
        let mut session = crate::model::Session::new(
            "local/x/HEAD..worktree".into(),
            crate::model::Repo {
                root: std::path::PathBuf::from("/repo"),
                host: None,
                owner: None,
                name: None,
                remote_url: None,
            },
            crate::model::Source::Local {
                base_spec: "HEAD".into(),
                head_spec: "worktree".into(),
                worktree: std::path::PathBuf::from("/repo"),
            },
            crate::model::RevRef {
                r#ref: Some("HEAD".into()),
                sha: "a".repeat(40),
            },
            crate::model::HeadRef {
                r#ref: None,
                sha: None,
                dirty: false,
            },
            "t".into(),
        );
        session.history.push(
            serde_json::from_value(serde_json::json!({
                "id": "c_1", "path": "a", "side": "head", "line": 1, "kind": "issue",
                "body": "v2", "anchor": { "base_sha": "a", "head_sha": null, "blob": null,
                    "context": { "before": [], "lines": [], "after": [] } },
                "stale": false, "commentable": { "github": true, "reason": null },
                "created_at": "t", "updated_at": "v2-time"
            }))
            .unwrap(),
        );
        let mut late = result(&["c_1"], &[], &[], DecisionStatus::NotApplicable);
        late.sent[0].version = Some("v1-time".into());
        apply(&mut session, late, true).unwrap();
        assert!(session.history[0].deliveries.is_empty(), "v2 never reached that sink");
    }

    #[test]
    fn summary_counts_and_names_reasons() {
        let r = result(&["a", "b", "c"], &[], &["d"], DecisionStatus::NotApplicable);
        assert_eq!(summary(&r), "3 sent, 1 skipped (outside diff), 0 failed");
        let r = result(&[], &[None], &[], DecisionStatus::Failed);
        assert_eq!(summary(&r), "0 sent, 1 failed, decision failed");
        let mut copy = result(&["a"], &[], &[], DecisionStatus::Sent);
        copy.mode = DeliveryMode::Copied;
        assert_eq!(summary(&copy), "1 copied, 0 failed, decision copied");
        copy.mode = DeliveryMode::Staged;
        assert_eq!(summary(&copy), "1 staged, 0 failed, decision staged");
    }
}
