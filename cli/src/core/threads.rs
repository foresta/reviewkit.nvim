use crate::error::{Error, ErrorCode, Result};
use crate::model::{Session, ThreadAction, ThreadActionKind};

/// An action that would leave the thread as GitHub already has it is dropped
/// instead of queued, so toggling twice leaves nothing to send.
pub fn set_resolved(session: &mut Session, thread_id: &str, resolved: bool) -> Result<()> {
    let thread = session
        .threads
        .iter()
        .find(|t| t.id == thread_id)
        .ok_or_else(|| Error::new(ErrorCode::ENoThread, format!("no such thread: {thread_id}")))?;
    let already = thread.resolved == resolved;
    session.thread_actions.retain(|a| a.thread_id != thread_id);
    if !already {
        session.thread_actions.push(ThreadAction {
            thread_id: thread_id.to_string(),
            action: if resolved {
                ThreadActionKind::Resolve
            } else {
                ThreadActionKind::Unresolve
            },
        });
    }
    Ok(())
}

pub fn prune_actions(session: &mut Session) {
    let threads = &session.threads;
    session.thread_actions.retain(|action| {
        threads
            .iter()
            .any(|t| t.id == action.thread_id && t.resolved != matches!(action.action, ThreadActionKind::Resolve))
    });
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::model::{HeadRef, Repo, RevRef, Side, Source, SubjectType, Thread};

    fn session_with(resolved: bool) -> Session {
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
            "2026-01-01T00:00:00.000000Z".into(),
        );
        session.threads = vec![Thread {
            id: "PRRT_1".into(),
            path: "src/a.rs".into(),
            side: Side::Head,
            line: Some(3),
            start_line: None,
            start_side: None,
            original_line: Some(3),
            resolved,
            outdated: false,
            subject_type: SubjectType::Line,
            comments: vec![],
        }];
        session
    }

    #[test]
    fn resolving_an_open_thread_queues_one_action() {
        let mut s = session_with(false);
        set_resolved(&mut s, "PRRT_1", true).unwrap();
        set_resolved(&mut s, "PRRT_1", true).unwrap();
        assert_eq!(s.thread_actions.len(), 1);
        assert_eq!(s.thread_actions[0].action, ThreadActionKind::Resolve);
    }

    #[test]
    fn undoing_a_queued_resolve_leaves_nothing_to_send() {
        let mut s = session_with(false);
        set_resolved(&mut s, "PRRT_1", true).unwrap();
        set_resolved(&mut s, "PRRT_1", false).unwrap();
        assert!(s.thread_actions.is_empty());
    }

    #[test]
    fn unresolving_a_resolved_thread_queues_unresolve() {
        let mut s = session_with(true);
        set_resolved(&mut s, "PRRT_1", false).unwrap();
        assert_eq!(s.thread_actions[0].action, ThreadActionKind::Unresolve);
    }

    #[test]
    fn unknown_thread_is_no_thread() {
        let mut s = session_with(false);
        let err = set_resolved(&mut s, "PRRT_9", true).unwrap_err();
        assert_eq!(err.code, ErrorCode::ENoThread);
    }

    #[test]
    fn prune_drops_actions_already_applied_or_orphaned() {
        let mut s = session_with(false);
        set_resolved(&mut s, "PRRT_1", true).unwrap();
        s.thread_actions.push(ThreadAction {
            thread_id: "PRRT_gone".into(),
            action: ThreadActionKind::Resolve,
        });
        prune_actions(&mut s);
        assert_eq!(s.thread_actions.len(), 1);
        s.threads[0].resolved = true;
        prune_actions(&mut s);
        assert!(s.thread_actions.is_empty());
    }
}
