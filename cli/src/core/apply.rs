use crate::core::time;
use crate::error::Result;
use crate::model::{
    DecisionStatus, Delivery, DeliveryMode, LastError, LastSubmit, Reason, SendStatus, Session, SinkResult, Source,
};

/// Whether delivered comments leave the queue. Only the destination a
/// session kind exists for consumes them by default: a PR's comments are
/// for GitHub even after an agent saw them, and a local review is for the
/// agent. Staged or copied text never consumes (decisions.md).
pub fn consumes(session: &Session, result: &SinkResult, requested: Option<bool>) -> bool {
    if result.mode != DeliveryMode::Sent {
        return false;
    }
    // GitHub keeps what it was sent and the github sink resends the whole
    // queue, so keeping its comments would post them twice.
    if result.sink == "github" {
        return true;
    }
    requested.unwrap_or(matches!(
        (&session.source, result.sink.as_str()),
        (Source::Local { .. }, "herdr")
    ))
}

/// Applies a sink's report to the queue (architecture D4).
pub fn apply(session: &mut Session, mut result: SinkResult, consume: Option<bool>) -> Result<SinkResult> {
    if result.at.is_empty() {
        result.at = time::now();
    }
    if result.summary.is_empty() {
        result.summary = summary(&result);
    }
    let consume = consumes(session, &result, consume);
    let edited_meanwhile = |comment: &crate::model::Comment, sent: &crate::model::Sent| {
        sent.version.as_deref().is_some_and(|v| v != comment.updated_at)
    };
    // An edit while the sink was sending means the delivered text is not
    // the comment's text any more: it stays queued as if not sent.
    // Entries for comments no longer queued (recovered and applied earlier)
    // are kept as reported.
    let edited: Vec<String> = result
        .sent
        .iter()
        .filter(|s| {
            session
                .comments
                .iter()
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
        if let Some(comment) = session.comments.iter_mut().find(|c| c.id == sent.comment_id) {
            comment.last_error = None;
            comment.deliveries.push(Delivery {
                sink: result.sink.clone(),
                at: result.at.clone(),
                mode: result.mode,
                url: sent.url.clone(),
            });
        }
    }
    if consume {
        let (gone, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut session.comments)
            .into_iter()
            .partition(|c| delivered.contains(&c.id));
        session.comments = kept;
        session.history.extend(gone);
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
    if result.decision == DecisionStatus::Sent && consume {
        session.decision = None;
    }
    session.last_submit = Some(LastSubmit {
        sink: result.sink.clone(),
        at: result.at.clone(),
        result: result.clone(),
    });
    Ok(result)
}

/// "3 sent, 1 skipped (outside diff), 0 failed"
pub fn summary(result: &SinkResult) -> String {
    let mut parts = vec![format!("{} sent", result.sent.len())];
    if !result.skipped.is_empty() {
        let mut reasons: Vec<&str> = result.skipped.iter().map(|s| reason_label(s.reason)).collect();
        reasons.sort_unstable();
        reasons.dedup();
        parts.push(format!("{} skipped ({})", result.skipped.len(), reasons.join(", ")));
    }
    parts.push(format!("{} failed", result.failed.len()));
    match result.decision {
        DecisionStatus::Sent => parts.push("decision sent".into()),
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
                    error: Error::new(ErrorCode::EGh, "boom"),
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
            attempted: None,
            summary: String::new(),
        }
    }

    #[test]
    fn summary_counts_and_names_reasons() {
        let r = result(&["a", "b", "c"], &[], &["d"], DecisionStatus::NotApplicable);
        assert_eq!(summary(&r), "3 sent, 1 skipped (outside diff), 0 failed");
        let r = result(&[], &[None], &[], DecisionStatus::Failed);
        assert_eq!(summary(&r), "0 sent, 1 failed, decision failed");
    }
}
