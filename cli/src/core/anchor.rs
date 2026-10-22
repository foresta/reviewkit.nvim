use std::collections::HashMap;

use crate::core::comments::{self, SideFile};
use crate::core::files;
use crate::error::Result;
use crate::ext::git::Repository;
use crate::model::{Anchor, Hunk, Session, Side};

/// `None` when an edit touches the range itself: the text the comment was
/// about is no longer what it was.
pub fn shift(hunks: &[Hunk], start: u32, line: u32) -> Option<(u32, u32)> {
    let mut delta: i64 = 0;
    for hunk in hunks {
        match classify(hunk, start, line) {
            Position::Above => delta += i64::from(hunk.head_count) - i64::from(hunk.base_count),
            Position::Below => {}
            Position::Touching => return None,
        }
    }
    apply_delta(start, line, delta)
}

/// Edits touching the range are ignored so the caller can still look for
/// the original text at the place the surrounding edits
/// would have moved it to.
pub fn shift_past_touching(hunks: &[Hunk], start: u32, line: u32) -> Option<(u32, u32)> {
    let delta = hunks
        .iter()
        .filter(|h| classify(h, start, line) == Position::Above)
        .map(|h| i64::from(h.head_count) - i64::from(h.base_count))
        .sum();
    apply_delta(start, line, delta)
}

#[derive(PartialEq, Eq)]
enum Position {
    Above,
    Below,
    Touching,
}

/// An insertion sits between `base_start` and `base_start + 1`; a deletion
/// or replacement covers `base_start ..= base_start + base_count - 1`.
fn classify(hunk: &Hunk, start: u32, line: u32) -> Position {
    if hunk.base_count == 0 {
        return if hunk.base_start < start {
            Position::Above
        } else if hunk.base_start < line {
            Position::Touching
        } else {
            Position::Below
        };
    }
    let old_first = hunk.base_start;
    let old_last = hunk.base_start + hunk.base_count - 1;
    if old_last < start {
        Position::Above
    } else if old_first > line {
        Position::Below
    } else {
        Position::Touching
    }
}

fn apply_delta(start: u32, line: u32, delta: i64) -> Option<(u32, u32)> {
    let shifted = |n: u32| u32::try_from(i64::from(n) + delta).ok().filter(|n| *n >= 1);
    Some((shifted(start)?, shifted(line)?))
}

/// Commentability is recomputed for every comment, not only the moved ones,
/// since the diff may have moved. Side files and blob diffs are read once
/// per (side, path) / (old, new), not once per comment.
pub fn reanchor(session: &mut Session, repo: &Repository<'_>) -> Result<()> {
    let mut sides: HashMap<(Side, String), Option<SideFile>> = HashMap::new();
    let mut diffs: HashMap<(String, String), Option<Vec<Hunk>>> = HashMap::new();
    for index in 0..session.comments.len() {
        if session.comments[index].reply_to.is_some() {
            continue;
        }
        // A rename moves the head path; comments follow it so they keep the
        // "path is the head-side Target path" rule and stay commentable.
        if let Some(renamed) = session
            .targets
            .iter()
            .find(|t| t.old_path.as_deref() == Some(session.comments[index].path.as_str()))
            && !session.targets.iter().any(|t| t.path == session.comments[index].path)
        {
            session.comments[index].path = renamed.path.clone();
        }
        let (side, path, start, line, old_anchor) = {
            let c = &session.comments[index];
            (c.side, c.path.clone(), c.start_line, c.line, c.anchor.clone())
        };
        let key = (side, path.clone());
        if !sides.contains_key(&key) {
            let file = files::read_side(session, repo, side, &path)?;
            sides.insert(key.clone(), file);
        }
        let file = sides.get(&key).and_then(Option::as_ref);
        let outcome = follow(&old_anchor, file, start.unwrap_or(line), line, |old, new| {
            diffs
                .entry((old.to_string(), new.to_string()))
                .or_insert_with(|| repo.diff_blobs(old, new).ok())
                .clone()
        });
        let target = session.targets.iter().find(|t| t.path == path);
        let empty = SideFile {
            blob: None,
            binary: false,
            too_large: false,
            lines: vec![],
        };
        let commentable = comments::commentable_on(
            target,
            file.unwrap_or(&empty),
            side,
            outcome.line,
            start.map(|_| outcome.start),
        );
        let comment = &mut session.comments[index];
        comment.commentable = commentable;
        if outcome.stale {
            comment.stale = true;
            continue;
        }
        comment.stale = false;
        if let Some(file) = file {
            comment.line = outcome.line;
            comment.start_line = start.map(|_| outcome.start);
            comment.anchor = Anchor {
                base_sha: session.base.sha.clone(),
                head_sha: session.head.sha.clone(),
                blob: file.blob.clone(),
                context: comments::context(&file.lines, outcome.start, outcome.line),
            };
        }
    }
    Ok(())
}

fn is_empty_blob(id: &str) -> bool {
    matches!(
        id,
        "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391" | "473a0f4c3be8a93681a267e3b1e9a7dcda1185436fe141f7749120a303721813"
    )
}

struct Followed {
    start: u32,
    line: u32,
    stale: bool,
}

/// The diff is only an optimisation: when it cannot be taken (no blob on either
/// side, old blob pruned from the object store) the text check alone
/// decides, at the current position. A text comment on a file that became
/// binary goes stale but keeps its anchor, so it can be followed again if
/// the file turns back into text.
fn follow(
    anchor: &Anchor,
    file: Option<&SideFile>,
    start: u32,
    line: u32,
    mut diff: impl FnMut(&str, &str) -> Option<Vec<Hunk>>,
) -> Followed {
    let stale = Followed {
        start,
        line,
        stale: true,
    };
    let Some(file) = file else { return stale };
    // Made on a binary or too-large file: there is no text to check, and
    // calling it stale would hold back a comment nothing contradicts. An
    // empty text file has no context either, but its virtual first line is
    // gone once content arrives.
    if anchor.context.lines.is_empty() && !anchor.blob.as_deref().is_some_and(is_empty_blob) {
        return Followed {
            start,
            line,
            stale: false,
        };
    }
    if file.binary || file.too_large {
        return stale;
    }
    let matches = |s: u32, l: u32| {
        file.lines.get(s as usize - 1..l as usize).is_some_and(|actual| {
            actual
                .iter()
                .map(String::as_str)
                .eq(anchor.context.lines.iter().map(String::as_str))
        })
    };
    // Stricter: the surrounding lines must match too (clipped at the file's
    // edges), so a rewritten block that happens to keep one common line is
    // not mistaken for the original.
    let matches_with_context = |s: u32, l: u32| {
        matches(s, l) && {
            let before_len = anchor.context.before.len();
            let start = (s as usize - 1).saturating_sub(before_len);
            let before = &file.lines[start..s as usize - 1];
            let after_end = (l as usize + anchor.context.after.len()).min(file.lines.len());
            let after = &file.lines[l as usize..after_end];
            before == anchor.context.before.as_slice() && after == anchor.context.after.as_slice()
        }
    };
    let at = |s: u32, l: u32| Followed {
        start: s,
        line: l,
        stale: false,
    };
    match (&anchor.blob, &file.blob) {
        (Some(old), Some(new)) if old == new => return at(start, line),
        (Some(old), Some(new)) => {
            if let Some(hunks) = diff(old, new) {
                if let Some((s, l)) = shift(&hunks, start, line)
                    && matches(s, l)
                {
                    return at(s, l);
                }
                // An edit touched the range, or the text differs where the
                // shift landed. The lines may still be intact at the place the
                // surrounding edits moved them to (e.g. only a trailing newline
                // was added).
                if let Some((s, l)) = shift_past_touching(&hunks, start, line)
                    && matches_with_context(s, l)
                {
                    return at(s, l);
                }
                return stale;
            }
        }
        _ => {}
    }
    if matches(start, line) { at(start, line) } else { stale }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::AnchorContext;

    fn h(bs: u32, bc: u32, hs: u32, hc: u32) -> Hunk {
        Hunk {
            base_start: bs,
            base_count: bc,
            head_start: hs,
            head_count: hc,
        }
    }

    #[test]
    fn edits_above_shift_the_range() {
        assert_eq!(shift(&[h(0, 0, 1, 2)], 5, 6), Some((7, 8)), "insert at top");
        assert_eq!(shift(&[h(2, 2, 1, 0)], 5, 6), Some((3, 4)), "delete two lines above");
        assert_eq!(
            shift(&[h(1, 1, 1, 3)], 5, 5),
            Some((7, 7)),
            "replace one line with three above"
        );
        assert_eq!(
            shift(&[h(1, 1, 1, 2), h(3, 0, 5, 1), h(10, 1, 11, 0)], 5, 6),
            Some((7, 8)),
            "several hunks accumulate; the one below is ignored"
        );
        assert_eq!(shift(&[h(1, 3, 1, 1)], 5, 6), Some((3, 4)), "a shrinking replace above");
    }

    #[test]
    fn edits_below_and_insertions_after_the_range_do_nothing() {
        assert_eq!(shift(&[h(9, 1, 9, 1)], 5, 6), Some((5, 6)));
        assert_eq!(
            shift(&[h(6, 0, 7, 3)], 5, 6),
            Some((5, 6)),
            "insert right after the range"
        );
        assert_eq!(
            shift(&[h(4, 0, 5, 1)], 5, 5),
            Some((6, 6)),
            "insert right before line 5 moves it"
        );
    }

    #[test]
    fn edits_touching_the_range_make_it_stale() {
        assert_eq!(shift(&[h(5, 1, 5, 1)], 5, 5), None, "the line itself changed");
        assert_eq!(shift(&[h(4, 3, 4, 1)], 5, 6), None, "a replace spanning the range");
        assert_eq!(shift(&[h(5, 0, 6, 1)], 5, 6), None, "insert inside a multi-line range");
        assert_eq!(shift(&[h(1, 10, 1, 0)], 5, 6), None, "everything deleted");
        assert_eq!(shift_past_touching(&[h(1, 1, 1, 2), h(5, 1, 6, 1)], 5, 5), Some((6, 6)));
    }

    fn anchor(blob: Option<&str>, lines: &[&str]) -> Anchor {
        Anchor {
            base_sha: "a".repeat(40),
            head_sha: None,
            blob: blob.map(str::to_string),
            context: AnchorContext {
                before: vec![],
                lines: lines.iter().map(|l| l.to_string()).collect(),
                after: vec![],
            },
        }
    }

    fn file(blob: Option<&str>, binary: bool, lines: &[&str]) -> SideFile {
        SideFile {
            blob: blob.map(str::to_string),
            binary,
            too_large: false,
            lines: lines.iter().map(|l| l.to_string()).collect(),
        }
    }

    fn no_diff(_: &str, _: &str) -> Option<Vec<Hunk>> {
        None
    }

    #[test]
    fn without_a_usable_diff_the_text_at_the_current_position_decides() {
        let a = anchor(Some("old"), &["x"]);
        let same = follow(&a, Some(&file(Some("new"), false, &["q", "x"])), 2, 2, no_diff);
        assert!(!same.stale);
        let moved = follow(&a, Some(&file(Some("new"), false, &["x", "q"])), 2, 2, no_diff);
        assert!(moved.stale, "no diff to follow with, and the text is gone from line 2");
        let unanchored = follow(&anchor(None, &["x"]), Some(&file(None, false, &["x"])), 1, 1, no_diff);
        assert!(!unanchored.stale);
    }

    #[test]
    fn shifting_past_the_end_of_the_file_is_stale() {
        let a = anchor(Some("old"), &["x"]);
        let f = file(Some("new"), false, &["only"]);
        let out = follow(&a, Some(&f), 1, 1, |_, _| Some(vec![h(0, 0, 1, 1)]));
        assert!(out.stale);
    }

    #[test]
    fn text_to_binary_is_stale_but_binary_to_binary_is_not() {
        let text = anchor(Some("old"), &["x"]);
        assert!(follow(&text, Some(&file(Some("new"), true, &[])), 1, 1, no_diff).stale);
        let bin = anchor(Some("old"), &[]);
        assert!(!follow(&bin, Some(&file(Some("new"), true, &[])), 1, 1, no_diff).stale);
    }

    #[test]
    fn a_comment_made_without_context_stays_on_its_line_when_the_file_becomes_text() {
        let a = anchor(Some("old"), &[]);
        let f = file(Some("new"), false, &["a", "b", "c"]);
        let out = follow(&a, Some(&f), 2, 2, |_, _| Some(vec![h(1, 1, 1, 3)]));
        assert!(!out.stale);
        assert_eq!((out.start, out.line), (2, 2));
        let without_diff = follow(&a, Some(&f), 2, 2, no_diff);
        assert!(!without_diff.stale);
    }

    #[test]
    fn a_touching_hunk_that_keeps_the_text_is_not_stale() {
        let a = anchor(Some("old"), &["last"]);
        let f = file(Some("new"), false, &["first", "last"]);
        // Adding a trailing newline rewrites the last line in git's eyes.
        let out = follow(&a, Some(&f), 2, 2, |_, _| Some(vec![h(2, 1, 2, 1)]));
        assert!(!out.stale);
        assert_eq!(out.line, 2);
    }

    #[test]
    fn a_rewritten_block_that_keeps_one_common_line_is_stale() {
        let mut a = anchor(Some("old"), &["}"]);
        a.context.before = vec!["fn f() {".into(), "  body".into()];
        a.context.after = vec![];
        let f = file(Some("new"), false, &["X1", "X2", "}", "X3"]);
        let out = follow(&a, Some(&f), 3, 3, |_, _| Some(vec![h(1, 3, 1, 4)]));
        assert!(out.stale, "the closing brace survived but its surroundings did not");
    }
}
