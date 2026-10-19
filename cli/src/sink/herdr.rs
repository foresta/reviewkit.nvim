use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::core::time;
use crate::error::{Error, ErrorCode, Result};
use crate::ext::herdr::{self, Agent, Herdr};
use crate::model::{
    AgentType, Comment, DecisionStatus, DeliveryMode, Failed, Sent, Session, Side, SinkResult, SubmitOptions,
};

pub const NAME: &str = "herdr";

#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Preview {
    pub target: String,
    pub prompt: String,
    pub stage: bool,
}

#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Candidate {
    pub pane_id: String,
    pub agent: String,
    pub agent_status: Option<String>,
    pub cwd: Option<String>,
    pub title: Option<String>,
}

/// The pane whose agent works in this session's worktree. Several agents
/// may share a name (one claude per repository), so the match is on cwd,
/// never on the agent name.
pub fn resolve_target(agents: &[Agent], worktree: &Path, match_foreground_cwd: bool) -> Result<String> {
    let worktree = worktree.to_string_lossy();
    let root = normalize(&worktree);
    let agents: Vec<&Agent> = agents.iter().filter(|a| a.pane_id.is_some()).collect();
    let matched: Vec<&Agent> = agents
        .iter()
        .copied()
        .filter(|a| {
            a.cwd.as_deref().is_some_and(|c| normalize(c) == root)
                || (match_foreground_cwd && a.foreground_cwd.as_deref().is_some_and(|c| normalize(c) == root))
        })
        .collect();
    match matched.as_slice() {
        [one] => Ok(one.pane_id.clone().expect("filtered above")),
        [] if agents.is_empty() => Err(Error::new(ErrorCode::EHerdr, "no running agent found")),
        // Nothing matches the repository: let the user pick from everything,
        // which is how one sends to an agent in another tab on purpose.
        [] => Err(ambiguous(
            "no agent is running in this repository",
            agents.iter().copied(),
        )),
        many => Err(ambiguous(
            "several agents are running in this repository",
            many.iter().copied(),
        )),
    }
}

fn ambiguous<'a>(message: &str, agents: impl Iterator<Item = &'a Agent>) -> Error {
    let candidates: Vec<Candidate> = agents
        .map(|a| Candidate {
            pane_id: a.pane_id.clone().unwrap_or_default(),
            agent: a.agent.clone(),
            agent_status: a.agent_status.clone(),
            cwd: a.cwd.clone(),
            title: a.terminal_title_stripped.clone(),
        })
        .collect();
    Error::new(ErrorCode::EAmbiguous, message)
        .hint("pick one and pass its pane_id as SubmitOptions.target")
        .details(serde_json::json!({ "candidates": candidates }))
}

fn normalize(path: &str) -> &str {
    path.trim_end_matches('/')
}

/// `@path#L3-L5` for claude (the `@` keeps the agent from reading a leading
/// path as a slash command), `path:3-5` for plain CLIs.
fn reference(comment: &Comment, agent_type: AgentType) -> String {
    let range = match comment.start_line {
        Some(start) if start != comment.line => match agent_type {
            AgentType::Claude => format!("#L{}-L{}", start, comment.line),
            AgentType::Plain => format!(":{}-{}", start, comment.line),
        },
        _ => match agent_type {
            AgentType::Claude => format!("#L{}", comment.line),
            AgentType::Plain => format!(":{}", comment.line),
        },
    };
    match agent_type {
        AgentType::Claude => format!("@{}{range}", comment.path),
        AgentType::Plain => format!("{}{range}", comment.path),
    }
}

/// A comment the agent already received is not sent again; text that was
/// only staged in its input may have been discarded there, so it is.
fn to_send(session: &Session) -> Vec<&Comment> {
    session
        .comments
        .iter()
        .filter(|c| {
            !c.deliveries
                .iter()
                .any(|d| d.sink == NAME && d.mode == DeliveryMode::Sent)
        })
        .collect()
}

/// An agent cannot open a GitHub thread, so a reply carries the thread's
/// first remark it answers.
fn reply_context(session: &Session, thread_id: &str) -> String {
    match session.threads.iter().find(|t| t.id == thread_id) {
        Some(thread) => {
            let first = thread.comments.first();
            let author = first.map(|c| c.author.as_str()).unwrap_or("?");
            let said = first
                .and_then(|c| c.body.lines().next())
                .unwrap_or("")
                .chars()
                .take(120)
                .collect::<String>();
            format!("reply to the review thread by {author}: \"{said}\"")
        }
        None => format!("reply to review thread {thread_id}"),
    }
}

/// header → decision body → numbered comments. Base-side and stale
/// comments carry a note because the agent sees only the current file.
pub fn format_prompt(session: &Session, opts: &SubmitOptions) -> String {
    let agent_type = opts.agent_type.unwrap_or(AgentType::Claude);
    let mut sections: Vec<String> = Vec::new();
    if let Some(header) = opts.header.as_deref().filter(|h| !h.trim().is_empty()) {
        sections.push(header.trim_end().to_string());
    }
    if let Some(decision) = &session.decision
        && !decision.body.trim().is_empty()
    {
        sections.push(decision.body.trim_end().to_string());
    }
    let blocks: Vec<String> = to_send(session)
        .into_iter()
        .enumerate()
        .map(|(i, c)| {
            let reply = c.reply_to.as_deref().map(|t| reply_context(session, t));
            let mut notes: Vec<&str> = Vec::new();
            if let Some(reply) = &reply {
                notes.push(reply);
            }
            if c.side == Side::Base {
                notes.push("refers to the base version of the file");
            }
            if c.stale {
                notes.push("the line may have shifted since the comment was written");
            }
            let mut head = format!("{}. {}", i + 1, reference(c, agent_type));
            if c.kind != crate::model::CommentKind::Issue {
                head.push_str(&format!(" ({})", kind_label(c.kind)));
            }
            if !notes.is_empty() {
                head.push_str(&format!(" [{}]", notes.join("; ")));
            }
            let mut block = vec![head, c.body.trim_end().to_string()];
            if let Some(suggestion) = c.suggestion.as_deref().filter(|s| !s.trim().is_empty()) {
                let fence = fence_for(suggestion);
                block.push(format!(
                    "Suggested replacement:\n{fence}\n{}\n{fence}",
                    suggestion.trim_end()
                ));
            }
            block.join("\n")
        })
        .collect();
    if !blocks.is_empty() {
        sections.push(blocks.join("\n\n"));
    }
    sections.join("\n\n")
}

/// One more backtick than the longest run inside, so a suggestion that
/// itself contains a code fence cannot close ours early.
fn fence_for(text: &str) -> String {
    let longest = text.split(|c| c != '`').map(str::len).max().unwrap_or(0);
    "`".repeat(longest.max(2) + 1)
}

fn kind_label(kind: crate::model::CommentKind) -> &'static str {
    use crate::model::CommentKind::*;
    match kind {
        Issue => "issue",
        Suggestion => "suggestion",
        Question => "question",
        Praise => "praise",
        Nit => "nit",
    }
}

pub fn preview(session: &Session, opts: &SubmitOptions, herdr: &dyn Herdr) -> Result<Preview> {
    if to_send(session).is_empty() && session.decision.as_ref().is_none_or(|d| d.body.trim().is_empty()) {
        return Err(Error::invalid(
            "nothing to submit: no comment the agent has not received yet, and no decision body",
        ));
    }
    let target = match &opts.target {
        Some(target) => target.clone(),
        None => resolve_target(
            &herdr::agents(herdr)?,
            &session.repo.root,
            opts.match_foreground_cwd.unwrap_or(true),
        )?,
    };
    Ok(Preview {
        target,
        prompt: format_prompt(session, opts),
        stage: opts.stage.unwrap_or(false),
    })
}

/// Everything before the send is a hard error; once herdr was asked to
/// deliver, the outcome is reported per comment so the queue can keep what
/// did not get through. An agent only receives text, so a Decision whose
/// body is empty (a bare `approve`) has nothing to send and stays, reported
/// as skipped.
pub fn submit(session: &Session, opts: &SubmitOptions, herdr: &dyn Herdr) -> Result<SinkResult> {
    let preview = preview(session, opts, herdr)?;
    let outcome = if preview.stage {
        herdr::send_text(herdr, &preview.target, &preview.prompt)
    } else {
        herdr::prompt(herdr, &preview.target, &preview.prompt)
    };
    let decision = match &session.decision {
        None => DecisionStatus::NotApplicable,
        Some(d) if d.body.trim().is_empty() => DecisionStatus::Skipped,
        Some(_) => DecisionStatus::Sent,
    };
    let result = match outcome {
        Ok(()) => SinkResult {
            sink: NAME.into(),
            at: time::now(),
            url: None,
            sent: to_send(session)
                .into_iter()
                .map(|c| Sent {
                    comment_id: c.id.clone(),
                    version: None,
                    remote_id: None,
                    url: None,
                })
                .collect(),
            failed: Vec::new(),
            skipped: Vec::new(),
            thread_actions: Vec::new(),
            decision,
            summary: String::new(),
            mode: if preview.stage {
                DeliveryMode::Staged
            } else {
                DeliveryMode::Sent
            },
            attempted: Some(to_send(session).iter().map(|c| c.id.clone()).collect()),
        },
        Err(error) if error.code == ErrorCode::EHerdrMissing => return Err(error),
        Err(error) => SinkResult {
            sink: NAME.into(),
            at: time::now(),
            url: None,
            sent: Vec::new(),
            failed: vec![Failed {
                comment_id: None,
                error,
            }],
            skipped: Vec::new(),
            thread_actions: Vec::new(),
            decision: if decision == DecisionStatus::Sent {
                DecisionStatus::Failed
            } else {
                decision
            },
            summary: String::new(),
            mode: DeliveryMode::Sent,
            attempted: Some(to_send(session).iter().map(|c| c.id.clone()).collect()),
        },
    };
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        Anchor, AnchorContext, CommentKind, Commentable, Decision, DecisionEvent, HeadRef, Repo, RevRef, Source,
    };
    use std::path::PathBuf;

    fn agent(pane: &str, cwd: &str, fg: &str) -> Agent {
        Agent {
            pane_id: Some(pane.into()),
            agent: "claude".into(),
            agent_status: Some("idle".into()),
            cwd: Some(cwd.into()),
            foreground_cwd: Some(fg.into()),
            terminal_title_stripped: None,
            workspace_id: None,
        }
    }

    #[test]
    fn target_resolution_matches_on_cwd_and_reports_candidates() {
        let agents = vec![agent("w1:p1", "/repo", "/repo"), agent("w2:p1", "/other", "/repo/sub")];
        assert_eq!(resolve_target(&agents, Path::new("/repo/"), false).unwrap(), "w1:p1");
        assert_eq!(resolve_target(&agents, Path::new("/repo/sub"), true).unwrap(), "w2:p1");
        let none = resolve_target(&agents, Path::new("/elsewhere"), true).unwrap_err();
        assert_eq!(none.code, ErrorCode::EAmbiguous);
        assert_eq!(none.details["candidates"].as_array().unwrap().len(), 2);
        let two = vec![
            agent("a", "/repo", "/repo"),
            agent("b", "/repo", "/repo"),
            agent("c", "/other", "/other"),
        ];
        let many = resolve_target(&two, Path::new("/repo"), false).unwrap_err();
        let ids: Vec<&str> = many.details["candidates"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["pane_id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, vec!["a", "b"], "only the matching agents are offered");
        let mut no_pane = agent("x", "/repo", "/repo");
        no_pane.pane_id = None;
        assert_eq!(
            resolve_target(&[no_pane, agent("y", "/repo", "/repo")], Path::new("/repo"), false).unwrap(),
            "y"
        );
        assert_eq!(
            resolve_target(&[], Path::new("/repo"), false).unwrap_err().code,
            ErrorCode::EHerdr
        );
    }

    fn session() -> Session {
        let at = "2026-01-02T03:04:05.000000Z".to_string();
        let mut s = Session::new(
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
                dirty: true,
            },
            at.clone(),
        );
        let comment = |id: &str, path: &str, line: u32, start: Option<u32>, kind: CommentKind, body: &str| Comment {
            id: id.into(),
            path: path.into(),
            side: Side::Head,
            line,
            start_line: start,
            kind,
            body: body.into(),
            suggestion: None,
            reply_to: None,
            anchor: Anchor {
                base_sha: "a".repeat(40),
                head_sha: None,
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
            created_at: at.clone(),
            updated_at: at.clone(),
        };
        s.comments = vec![
            comment("c_1", "src/a.rs", 3, None, CommentKind::Issue, "Handle the empty case."),
            comment(
                "c_2",
                "src/b.rs",
                10,
                Some(8),
                CommentKind::Nit,
                "Rename to `count`.\nIt reads better.",
            ),
        ];
        s.comments[1].side = Side::Base;
        s.comments[1].stale = true;
        s.comments[1].suggestion = Some("let count = 0;".into());
        s.decision = Some(Decision {
            event: DecisionEvent::RequestChanges,
            body: "Two things before merge.".into(),
            updated_at: at,
        });
        s
    }

    #[test]
    fn prompt_format_for_claude() {
        let opts = SubmitOptions {
            header: Some("Review feedback:".into()),
            ..Default::default()
        };
        insta::assert_snapshot!(format_prompt(&session(), &opts));
    }

    #[test]
    fn suggestion_fence_grows_past_inner_backticks() {
        assert_eq!(fence_for("let x = 1;"), "```");
        assert_eq!(fence_for("```rust\nx\n```"), "````");
        assert_eq!(fence_for("`````"), "``````");
    }

    #[test]
    fn prompt_format_for_plain_agents_has_no_at_prefix() {
        let opts = SubmitOptions {
            agent_type: Some(AgentType::Plain),
            ..Default::default()
        };
        let prompt = format_prompt(&session(), &opts);
        assert!(prompt.contains("1. src/a.rs:3\n"));
        assert!(prompt.contains("2. src/b.rs:8-10 (nit)"));
        assert!(!prompt.contains('@'));
    }
}
