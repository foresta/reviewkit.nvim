use crate::core::commentable;
use crate::core::time;
use crate::error::{Error, ErrorCode, Result};
use crate::model::{
    Anchor, AnchorContext, Comment, CommentInput, CommentPatch, Commentable, Reason, Session, Side, Target,
};

/// Lines of context stored on each side of the commented range, enough to
/// recognise the lines again after they move.
const CONTEXT_LINES: usize = 2;

/// The side's file as the comment sees it: `None` when the path does not
/// exist on that side (deleted on head, added on base).
pub struct SideFile {
    pub blob: Option<String>,
    /// Binary content has no lines to anchor to; the comment is still
    /// queued (for an agent) with `Reason::Binary`.
    pub binary: bool,
    /// Over `TOO_LARGE_BYTES`: like binary, never read, so `lines` is empty.
    pub too_large: bool,
    pub lines: Vec<String>,
}

/// GitHub rejects an empty comment and an agent gets nothing to act on; a
/// suggestion alone is still a comment, even an empty one (delete the lines).
fn check_text(body: &str, suggestion: Option<&str>) -> Result<()> {
    if body.trim().is_empty() && suggestion.is_none() {
        return Err(Error::invalid("the comment is empty").hint("write a body or a suggestion"));
    }
    Ok(())
}

pub fn add(session: &mut Session, input: CommentInput, file: Option<SideFile>) -> Result<String> {
    check_text(&input.body, input.suggestion.as_deref())?;
    if target_for(session, &input.path).is_none()
        && let Some(renamed) = session
            .targets
            .iter()
            .find(|t| t.old_path.as_deref() == Some(&input.path))
    {
        return Err(Error::invalid(format!(
            "{} was renamed; comment on {} instead",
            input.path, renamed.path
        ))
        .hint("comments use the head-side path on both sides"));
    }
    if let Some(thread_id) = &input.reply_to {
        // Queued, it would fail every submit until removed by hand.
        if !session.threads.iter().any(|t| &t.id == thread_id) {
            return Err(Error::new(ErrorCode::ENoThread, format!("no such thread: {thread_id}"))
                .hint("refresh the session to see the current threads"));
        }
        return Ok(push_reply(session, input, file));
    }
    let file = file.ok_or_else(|| not_commentable(&input.path, "the path does not exist on that side"))?;
    check_range(&input.path, input.line, input.start_line, &file)?;
    let now = time::now();
    let comment = Comment {
        id: format!("c_{}", ulid::Ulid::generate()),
        path: input.path.clone(),
        side: input.side,
        line: input.line,
        start_line: single_line(input.start_line, input.line),
        kind: input.kind,
        body: input.body,
        suggestion: input.suggestion,
        reply_to: input.reply_to,
        anchor: anchor(session, &file, input.line, input.start_line),
        stale: false,
        commentable: commentable_on(
            target_for(session, &input.path),
            &file,
            input.side,
            input.line,
            input.start_line,
        ),
        last_error: None,
        deliveries: Vec::new(),
        created_at: now.clone(),
        updated_at: now,
    };
    let id = comment.id.clone();
    session.comments.push(comment);
    Ok(id)
}

/// A reply goes into its thread, not onto a line, so the line it carries is
/// only where the thread was shown: the file may be gone or shorter by now
/// (outdated threads), and that is no reason to refuse it.
fn push_reply(session: &mut Session, input: CommentInput, file: Option<SideFile>) -> String {
    let now = time::now();
    let context = file
        .as_ref()
        .map(|f| context(&f.lines, input.start_line.unwrap_or(input.line), input.line))
        .unwrap_or_default();
    let comment = Comment {
        id: format!("c_{}", ulid::Ulid::generate()),
        path: input.path,
        side: input.side,
        line: input.line.max(1),
        start_line: None,
        kind: input.kind,
        body: input.body,
        suggestion: input.suggestion,
        reply_to: input.reply_to,
        anchor: Anchor {
            base_sha: session.base.sha.clone(),
            head_sha: session.head.sha.clone(),
            blob: file.and_then(|f| f.blob),
            context,
        },
        stale: false,
        commentable: Commentable {
            github: true,
            reason: None,
        },
        last_error: None,
        deliveries: Vec::new(),
        created_at: now.clone(),
        updated_at: now,
    };
    let id = comment.id.clone();
    session.comments.push(comment);
    id
}

/// `file` is the side's current content; `None` when the path no longer
/// exists there, which only matters if the line moves.
pub fn update(session: &mut Session, id: &str, patch: CommentPatch, file: Option<SideFile>) -> Result<()> {
    let index = find(session, id)?;
    {
        let comment = &session.comments[index];
        let body = patch.body.as_deref().unwrap_or(&comment.body);
        let suggestion = match &patch.suggestion {
            Some(s) => s.as_deref(),
            None => comment.suggestion.as_deref(),
        };
        check_text(body, suggestion)?;
    }
    if session.comments[index].reply_to.is_some() && patch_moves_line(&patch) {
        return Err(Error::invalid("a reply belongs to its thread and has no line to move"));
    }
    // Any explicit position re-anchors, even the same number: that is how a
    // stale comment is confirmed against the current text.
    let moved = patch_moves_line(&patch);
    let (line, start_line) = {
        let comment = &session.comments[index];
        (
            patch.line.unwrap_or(comment.line),
            patch.start_line.unwrap_or(comment.start_line),
        )
    };
    let (anchor, commentable) = if moved {
        let comment = &session.comments[index];
        let file = file.ok_or_else(|| not_commentable(&comment.path, "the path does not exist on that side"))?;
        check_range(&comment.path, line, start_line, &file)?;
        let target = target_for(session, &comment.path);
        (
            Some(anchor(session, &file, line, start_line)),
            Some(commentable_on(target, &file, comment.side, line, start_line)),
        )
    } else {
        (None, None)
    };
    let comment = &mut session.comments[index];
    let text_changed = patch.body.as_ref().is_some_and(|b| *b != comment.body)
        || patch.suggestion.as_ref().is_some_and(|s| *s != comment.suggestion)
        || patch.kind.is_some_and(|k| k != comment.kind);
    comment.line = line;
    comment.start_line = single_line(start_line, line);
    if let Some(kind) = patch.kind {
        comment.kind = kind;
    }
    if let Some(body) = patch.body {
        comment.body = body;
    }
    if let Some(suggestion) = patch.suggestion {
        comment.suggestion = suggestion;
    }
    if text_changed {
        comment.deliveries.clear();
    }
    if let Some(anchor) = anchor {
        comment.anchor = anchor;
        comment.stale = false;
    }
    if let Some(commentable) = commentable {
        comment.commentable = commentable;
    }
    comment.updated_at = time::now();
    Ok(())
}

/// Whether the patch needs the side file read (a git call) at all.
pub fn patch_moves_line(patch: &CommentPatch) -> bool {
    patch.line.is_some() || patch.start_line.is_some()
}

pub fn remove(session: &mut Session, id: &str) -> Result<Comment> {
    let index = find(session, id)?;
    Ok(session.comments.remove(index))
}

/// Moves comments delivered (for real, not staged or copied) out of the
/// queue, optionally only those delivered through one sink.
pub fn clear_delivered(session: &mut Session, sink: Option<&str>, keep: &[String]) -> Vec<String> {
    let delivered = |c: &Comment| {
        !keep.contains(&c.id)
            && c.deliveries
                .iter()
                .any(|d| d.mode == crate::model::DeliveryMode::Sent && sink.is_none_or(|s| d.sink == s))
    };
    let (gone, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut session.comments).into_iter().partition(delivered);
    session.comments = kept;
    let moved = gone.iter().map(|c| c.id.clone()).collect();
    crate::core::apply::retire(&mut session.history, gone);
    moved
}

/// Brings a comment back with its deliveries, so no sink sends it twice.
/// A comment already in the queue stays where it is. `final_sink` says
/// whether a sink is where this session's comments end up: only a comment
/// consumed there is refused (an agent given a PR comment with `consume`
/// does not post it, and GitHub may still need it).
pub fn restore(session: &mut Session, id: &str, final_sink: impl Fn(&str) -> bool) -> Result<()> {
    if session.comments.iter().any(|c| c.id == id) {
        return Ok(());
    }
    let index = session
        .history
        .iter()
        .position(|c| c.id == id)
        .ok_or_else(|| Error::new(ErrorCode::ENoComment, format!("no such comment in the history: {id}")))?;
    if let Some(d) = session.history[index]
        .deliveries
        .iter()
        .find(|d| d.consumed && final_sink(&d.sink))
    {
        return Err(Error::invalid(format!(
            "{id} already reached its destination ({}); queue a new comment instead",
            d.sink
        )));
    }
    let comment = session.history.remove(index);
    session.comments.push(comment);
    Ok(())
}

/// The session a sink sees for a selection: only the chosen queued
/// comments, no decision and no thread actions. `None` without a selection.
/// The result is applied to the real session.
pub fn narrowed(session: &Session, ids: Option<&[String]>) -> Result<Option<Session>> {
    let Some(ids) = ids else {
        return Ok(None);
    };
    if ids.is_empty() {
        return Err(Error::invalid("nothing selected: comment_ids is empty"));
    }
    for id in ids {
        let queued = session.comments.iter().any(|c| &c.id == id);
        let sent = session.history.iter().any(|c| &c.id == id);
        if !queued && !sent {
            return Err(Error::new(ErrorCode::ENoComment, format!("no such comment: {id}")));
        }
    }
    let mut view = session.clone();
    view.comments.retain(|c| ids.contains(&c.id));
    // Through any sink: the history does not say this one would deliver them.
    if view.comments.is_empty() {
        return Err(
            Error::invalid("nothing to submit: the selected comments already left the queue")
                .details(serde_json::json!({ "empty": true })),
        );
    }
    view.decision = None;
    view.thread_actions.clear();
    view.history.clear();
    Ok(Some(view))
}

pub fn find(session: &Session, id: &str) -> Result<usize> {
    session
        .comments
        .iter()
        .position(|c| c.id == id)
        .ok_or_else(|| Error::new(ErrorCode::ENoComment, format!("no such comment: {id}")))
}

/// Comment paths are head-side Target paths on both sides, so one lookup
/// serves base and head alike (base content is resolved via `old_path`).
fn target_for<'a>(session: &'a Session, path: &str) -> Option<&'a Target> {
    session.targets.iter().find(|t| t.path == path)
}

/// A range of one line is a single-line comment; keeping both forms would
/// make sinks and the view tell them apart in each place.
fn single_line(start_line: Option<u32>, line: u32) -> Option<u32> {
    start_line.filter(|s| *s != line)
}

/// E_NOT_COMMENTABLE is reserved for lines that cannot exist; a line outside
/// the diff is accepted and merely flagged (D4). A malformed range is the
/// caller's mistake (E_INVALID). Binary and too large files have no line
/// count to check, and an empty file still has the one virtual line an
/// editor shows.
fn check_range(path: &str, line: u32, start_line: Option<u32>, file: &SideFile) -> Result<()> {
    if let Some(start) = start_line
        && (start == 0 || start > line)
    {
        return Err(Error::invalid(format!(
            "start_line {start} must be between 1 and line {line}"
        )));
    }
    if line == 0 {
        return Err(Error::invalid("line must be 1 or greater"));
    }
    let len = file.lines.len().max(1);
    if !file.binary && !file.too_large && line as usize > len {
        return Err(not_commentable(
            path,
            &format!("line {line} is outside the file (1..={len})"),
        ));
    }
    Ok(())
}

fn not_commentable(path: &str, why: &str) -> Error {
    Error::new(ErrorCode::ENotCommentable, format!("{path}: {why}"))
}

fn anchor(session: &Session, file: &SideFile, line: u32, start_line: Option<u32>) -> Anchor {
    Anchor {
        base_sha: session.base.sha.clone(),
        head_sha: session.head.sha.clone(),
        blob: file.blob.clone(),
        context: context(&file.lines, start_line.unwrap_or(line), line),
    }
}

/// 1-based closed range; `before` / `after` are clipped at the file edges.
pub fn context(lines: &[String], start: u32, end: u32) -> AnchorContext {
    let start = (start.max(1) as usize - 1).min(lines.len());
    let end = (end as usize).min(lines.len()).max(start);
    let before_from = start.saturating_sub(CONTEXT_LINES);
    let after_to = (end + CONTEXT_LINES).min(lines.len());
    AnchorContext {
        before: lines[before_from..start].to_vec(),
        lines: lines[start..end].to_vec(),
        after: lines[end..after_to].to_vec(),
    }
}

/// GitHub accepts a comment only on a changed line or its 3 lines of
/// context, on a non-binary file in the diff.
pub fn commentable_on(
    target: Option<&Target>,
    file: &SideFile,
    side: Side,
    line: u32,
    start_line: Option<u32>,
) -> Commentable {
    if file.binary || target.is_some_and(|t| t.binary) {
        return Commentable {
            github: false,
            reason: Some(Reason::Binary),
        };
    }
    if file.too_large || target.is_some_and(|t| t.too_large) {
        return Commentable {
            github: false,
            reason: Some(Reason::TooLarge),
        };
    }
    let Some(target) = target else {
        return Commentable {
            github: false,
            reason: Some(Reason::OutsideDiff),
        };
    };
    let ranges = match side {
        Side::Base => &target.commentable.base,
        Side::Head => &target.commentable.head,
    };
    let start = start_line.unwrap_or(line);
    let inside = (start..=line).all(|l| commentable::contains(ranges, l));
    Commentable {
        github: inside,
        reason: (!inside).then_some(Reason::OutsideDiff),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{CommentableRanges, Stats, TargetReason, TargetStatus};

    fn lines(n: usize) -> Vec<String> {
        (1..=n).map(|i| format!("l{i}")).collect()
    }

    fn text(n: usize) -> SideFile {
        SideFile {
            blob: None,
            binary: false,
            too_large: false,
            lines: lines(n),
        }
    }

    fn binary() -> SideFile {
        SideFile {
            blob: None,
            binary: true,
            too_large: false,
            lines: vec![],
        }
    }

    fn target(binary: bool, head: Vec<[u32; 2]>) -> Target {
        Target {
            id: "t_1".into(),
            path: "a.rs".into(),
            old_path: None,
            status: TargetStatus::Modified,
            reason: TargetReason::Changed,
            base_blob: None,
            head_blob: None,
            binary,
            too_large: false,
            viewed: false,
            stats: Stats::default(),
            hunks: vec![],
            commentable: CommentableRanges { base: vec![], head },
            range: None,
        }
    }

    #[test]
    fn context_is_clipped_at_both_ends() {
        let ctx = context(&lines(5), 1, 2);
        assert!(ctx.before.is_empty());
        assert_eq!(ctx.lines, vec!["l1", "l2"]);
        assert_eq!(ctx.after, vec!["l3", "l4"]);
        let ctx = context(&lines(5), 5, 5);
        assert_eq!(ctx.before, vec!["l3", "l4"]);
        assert_eq!(ctx.lines, vec!["l5"]);
        assert!(ctx.after.is_empty());
        let ctx = context(&[], 1, 1);
        assert!(ctx.before.is_empty() && ctx.lines.is_empty() && ctx.after.is_empty());
    }

    #[test]
    fn github_commentability_needs_the_whole_range_inside_the_diff() {
        let t = target(false, vec![[4, 10]]);
        let f = text(20);
        assert!(commentable_on(Some(&t), &f, Side::Head, 5, None).github);
        assert!(commentable_on(Some(&t), &f, Side::Head, 10, Some(4)).github);
        let outside = commentable_on(Some(&t), &f, Side::Head, 10, Some(3));
        assert_eq!((outside.github, outside.reason), (false, Some(Reason::OutsideDiff)));
        assert_eq!(
            commentable_on(Some(&t), &f, Side::Base, 5, None).reason,
            Some(Reason::OutsideDiff)
        );
        assert_eq!(
            commentable_on(None, &f, Side::Head, 5, None).reason,
            Some(Reason::OutsideDiff)
        );
        assert_eq!(
            commentable_on(Some(&target(true, vec![[1, 9]])), &f, Side::Head, 5, None).reason,
            Some(Reason::Binary)
        );
        assert_eq!(
            commentable_on(None, &binary(), Side::Head, 5, None).reason,
            Some(Reason::Binary)
        );
    }

    #[test]
    fn range_check_separates_impossible_lines_from_malformed_ranges() {
        assert!(check_range("a", 1, None, &text(3)).is_ok());
        assert!(check_range("a", 3, Some(1), &text(3)).is_ok());
        assert_eq!(
            check_range("a", 4, None, &text(3)).unwrap_err().code,
            ErrorCode::ENotCommentable
        );
        assert_eq!(
            check_range("a", 0, None, &text(3)).unwrap_err().code,
            ErrorCode::EInvalid
        );
        assert_eq!(
            check_range("a", 2, Some(3), &text(3)).unwrap_err().code,
            ErrorCode::EInvalid
        );
        assert!(
            check_range("a", 1, None, &text(0)).is_ok(),
            "an empty file has one virtual line"
        );
        assert!(check_range("a", 999, None, &binary()).is_ok());
    }
}
