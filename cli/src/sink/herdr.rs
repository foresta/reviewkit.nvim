use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::core::time;
use crate::error::{Error, ErrorCode, Result};
use crate::ext::herdr::{self, Agent, Herdr};
use crate::model::{
    AgentType, Comment, DecisionStatus, DeliveryMode, Failed, HerdrOptions, Sent, Session, Side, SinkResult,
    SinkTarget, Source, SubmitOptions, SubmitPreview,
};

pub const NAME: &str = "herdr";

#[derive(Debug, Serialize, Deserialize)]
struct Request {
    target: String,
    prompt: String,
    stage: bool,
    #[serde(skip)]
    elsewhere: bool,
}

/// A function rather than a git handle so the formatting stays testable
/// without a repository.
pub type CheckoutHead<'a> = &'a dyn Fn(&Path) -> Option<String>;

/// The pane whose agent works in this session's worktree. Several agents
/// may share a name (one claude per repository), so the match is on cwd,
/// never on the agent name.
pub fn resolve_target(agents: &[Agent], worktree: &Path, match_foreground_cwd: bool) -> Result<String> {
    let root = real(&worktree.to_string_lossy());
    let agents: Vec<&Agent> = agents.iter().filter(|a| a.pane_id.is_some()).collect();
    let dirs = |a: &Agent| -> Vec<PathBuf> {
        let mut dirs: Vec<PathBuf> = a.cwd.as_deref().map(real).into_iter().collect();
        if match_foreground_cwd {
            dirs.extend(a.foreground_cwd.as_deref().map(real));
        }
        dirs
    };
    // An agent started at the root wins over one in a subdirectory.
    let at_root: Vec<&Agent> = agents.iter().copied().filter(|a| dirs(a).contains(&root)).collect();
    let matched = if at_root.is_empty() {
        agents
            .iter()
            .copied()
            .filter(|a| dirs(a).iter().any(|d| inside_checkout(d, &root)))
            .collect()
    } else {
        at_root
    };
    match matched.as_slice() {
        [one] => Ok(one.pane_id.clone().expect("filtered above")),
        [] if agents.is_empty() => Err(Error::tool("herdr", ErrorCode::ETool, "no running agent found")),
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
    let candidates: Vec<SinkTarget> = agents
        .map(|a| SinkTarget {
            id: a.pane_id.clone().unwrap_or_default(),
            name: Some(a.agent.clone()),
            status: a.agent_status.clone(),
            cwd: a.cwd.clone(),
            title: a.terminal_title_stripped.clone(),
        })
        .collect();
    Error::new(ErrorCode::EAmbiguous, message)
        .hint("pick one and pass its id as the target option")
        .details(serde_json::json!({ "candidates": candidates }))
}

/// Under the root and in the same checkout: a directory on the way down
/// that has its own `.git` is another repository or worktree (a nested
/// agent worktree, a submodule) whose agent must not get this review.
fn inside_checkout(dir: &Path, root: &Path) -> bool {
    if !dir.starts_with(root) {
        return false;
    }
    dir.ancestors()
        .take_while(|d| *d != root)
        .all(|d| !d.join(".git").exists())
}

/// herdr reports the shell's view of the cwd, which may go through a
/// symlink the worktree path does not.
fn real(path: &str) -> PathBuf {
    let trimmed = Path::new(path.trim_end_matches('/'));
    std::fs::canonicalize(trimmed).unwrap_or_else(|_| trimmed.to_path_buf())
}

/// `@path#L3-L5` for claude (the `@` keeps the agent from reading a leading
/// path as a slash command), `path:3-5` for plain CLIs. An `@` mention ends
/// at whitespace, so a path with spaces is written in backticks instead.
fn reference(comment: &Comment, agent_type: AgentType) -> String {
    if agent_type == AgentType::Claude && comment.path.contains(char::is_whitespace) {
        let plain = reference(comment, AgentType::Plain);
        let range = &plain[comment.path.len()..];
        return format!("`{}`{range}", comment.path);
    }
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

/// A wide base range would otherwise flood the agent's prompt.
const MAX_QUOTED_LINES: usize = 40;

/// Text only, and only once: a bare event has nothing for an agent to read,
/// and a decision the agent already received would just repeat.
fn decision_to_send(session: &Session) -> Option<&crate::model::Decision> {
    session.decision.as_ref().filter(|d| {
        !d.body.trim().is_empty()
            && !d
                .deliveries
                .iter()
                .any(|x| x.sink == NAME && x.mode == DeliveryMode::Sent)
    })
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
pub fn format_prompt(session: &Session, opts: &HerdrOptions, agent_head: Option<&str>) -> String {
    let agent_type = opts.agent_type.unwrap_or(AgentType::Claude);
    let mut sections: Vec<String> = Vec::new();
    if let Some(header) = opts.header.as_deref().filter(|h| !h.trim().is_empty()) {
        sections.push(header.trim_end().to_string());
    }
    if let Some(context) = pr_context(session, agent_head) {
        sections.push(context);
    }
    if let Some(decision) = decision_to_send(session) {
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
            // The agent only has the current files; the base lines a comment
            // is about exist nowhere it can read, so they travel with it.
            if c.side == Side::Base && c.reply_to.is_none() && !c.anchor.context.lines.is_empty() {
                let lines = &c.anchor.context.lines;
                let shown = lines.len().min(MAX_QUOTED_LINES);
                let quoted = lines[..shown].join("\n");
                let fence = fence_for(&quoted);
                let more = match lines.len() - shown {
                    0 => String::new(),
                    n => format!("\n({n} more lines)"),
                };
                block.push(format!("Base lines it refers to:\n{fence}\n{quoted}\n{fence}{more}"));
            }
            if c.suggestion.as_deref().is_some_and(|s| s.trim().is_empty()) {
                block.push("Suggested change: delete these lines.".to_string());
            } else if let Some(suggestion) = c.suggestion.as_deref() {
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

/// The agent is found by the user's clone, which may be on another commit
/// than the one the comments' line numbers refer to.
fn pr_context(session: &Session, agent_head: Option<&str>) -> Option<String> {
    let Source::Pr {
        number,
        url,
        head_ref,
        worktree,
        ..
    } = &session.source
    else {
        return None;
    };
    let sha = session.head.sha.as_deref()?;
    let mut lines = vec![
        format!("Review of pull request #{number} ({url}), branch `{head_ref}` at {sha}."),
        format!(
            "Line numbers refer to that commit, checked out at `{}`.",
            worktree.display()
        ),
    ];
    if let Some(other) = agent_head.filter(|h| *h != sha) {
        lines.push(format!(
            "Your checkout is at {}, not that commit: check the lines there before changing anything.",
            short(other)
        ));
    }
    Some(lines.join("\n"))
}

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(12)]
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

fn request(
    session: &Session,
    opts: &HerdrOptions,
    herdr: &dyn Herdr,
    checkout_head: CheckoutHead<'_>,
) -> Result<Request> {
    if to_send(session).is_empty() && decision_to_send(session).is_none() {
        return Err(Error::invalid(
            "nothing to submit: the agent already has every comment and the decision, or the decision has no body",
        ));
    }
    let is_pr = matches!(session.source, Source::Pr { .. });
    // With a target given, the list only serves the checkout note; a send
    // that worked without it must not fail on it.
    let agents = match (&opts.target, is_pr) {
        (None, _) => herdr::agents(herdr)?,
        (Some(_), true) => herdr::agents(herdr).unwrap_or_default(),
        (Some(_), false) => Vec::new(),
    };
    let target = match &opts.target {
        Some(target) => target.clone(),
        None => resolve_target(&agents, &session.repo.root, opts.match_foreground_cwd.unwrap_or(true))?,
    };
    // The directory the agent was matched by: a shell cwd outside the
    // repository with the agent's process inside it matches on the latter.
    let agent_head = if is_pr {
        let root = real(&session.repo.root.to_string_lossy());
        agents
            .iter()
            .find(|a| a.pane_id.as_deref() == Some(target.as_str()))
            .and_then(|a| {
                let mut dirs: Vec<PathBuf> = a.cwd.as_deref().map(real).into_iter().collect();
                if opts.match_foreground_cwd.unwrap_or(true) {
                    dirs.extend(a.foreground_cwd.as_deref().map(real));
                }
                let inside = dirs.iter().find(|d| **d == root || inside_checkout(d, &root)).cloned();
                inside.or_else(|| dirs.into_iter().next())
            })
            .and_then(|dir| checkout_head(&dir))
    } else {
        None
    };
    let elsewhere = session
        .head
        .sha
        .as_deref()
        .is_some_and(|sha| agent_head.as_deref().is_some_and(|h| h != sha));
    Ok(Request {
        target,
        prompt: format_prompt(session, opts, agent_head.as_deref()),
        stage: opts.stage.unwrap_or(false),
        elsewhere,
    })
}

pub fn preview(
    session: &Session,
    opts: &HerdrOptions,
    herdr: &dyn Herdr,
    checkout_head: CheckoutHead<'_>,
) -> Result<SubmitPreview> {
    let request = request(session, opts, herdr, checkout_head)?;
    Ok(SubmitPreview {
        sink: NAME.into(),
        comment_ids: to_send(session).iter().map(|c| c.id.clone()).collect(),
        body_comment_ids: vec![],
        skipped: vec![],
        replies: vec![],
        thread_actions: vec![],
        decision: decision_to_send(session).map(|d| d.event),
        text: Some(request.prompt.clone()),
        target: Some(request.target.clone()),
        mode: if request.stage {
            DeliveryMode::Staged
        } else {
            DeliveryMode::Sent
        },
        raw: serde_json::to_value(&request).unwrap_or_default(),
        digest: None,
    })
}

/// Everything before the send is a hard error; once herdr was asked to
/// deliver, the outcome is reported per comment so the queue can keep what
/// did not get through. An agent only receives text, so a Decision whose
/// body is empty (a bare `approve`) has nothing to send and stays, reported
/// as skipped.
pub fn submit(
    session: &Session,
    opts: &HerdrOptions,
    herdr: &dyn Herdr,
    checkout_head: CheckoutHead<'_>,
) -> Result<Submitted> {
    let preview = request(session, opts, herdr, checkout_head)?;
    let outcome = if preview.stage {
        herdr::send_text(herdr, &preview.target, &preview.prompt)
    } else {
        herdr::prompt(herdr, &preview.target, &preview.prompt)
    };
    let decision = match (&session.decision, decision_to_send(session)) {
        (None, _) => DecisionStatus::NotApplicable,
        (Some(_), None) => DecisionStatus::Skipped,
        (Some(_), Some(_)) => DecisionStatus::Sent,
    };
    let decision_version = session.decision.as_ref().map(|d| d.updated_at.clone());
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
            decision_version: decision_version.clone(),
            attempted: Some(to_send(session).iter().map(|c| c.id.clone()).collect()),
        },
        Err(error) if error.code == ErrorCode::EToolMissing => return Err(error),
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
            decision_version: decision_version.clone(),
            attempted: Some(to_send(session).iter().map(|c| c.id.clone()).collect()),
        },
    };
    Ok(Submitted {
        result,
        elsewhere: preview.elsewhere,
    })
}

pub struct Submitted {
    pub result: SinkResult,
    pub elsewhere: bool,
}

/// The note is added last: the summary is otherwise derived from fields the
/// caller may still change (a selection skips the decision).
fn finish_summary(result: &mut SinkResult, elsewhere: bool) {
    result.summary = crate::core::apply::summary(result);
    if elsewhere {
        result.summary = format!(
            "{}; the agent's checkout is not at the pull request head",
            result.summary
        );
    }
}

/// `None` when git or the directory gives no answer: then nothing is said
/// about the agent's checkout rather than a guess.
fn checkout_head_with(ctx: &crate::context::Context) -> impl Fn(&Path) -> Option<String> + '_ {
    let git = crate::ext::git::SystemGit::new(ctx.tools.git.as_deref()).ok();
    move |dir: &Path| {
        let git = git.as_ref()?;
        crate::ext::git::Repository::discover(git, dir).ok()?.rev_parse("HEAD")
    }
}

pub struct HerdrSink;

impl super::Sink for HerdrSink {
    fn name(&self) -> &'static str {
        NAME
    }

    fn info(&self, ctx: &crate::context::Context) -> super::SinkInfo {
        super::tool_info(NAME, ctx.tools.herdr.as_deref(), "herdr")
    }

    /// A local review is for the agent; a PR's comments stay for GitHub
    /// even after an agent saw them.
    fn consumes(&self, source: &Source, requested: Option<bool>) -> bool {
        requested.unwrap_or(matches!(source, Source::Local { .. }))
    }

    fn targets(&self, ctx: &crate::context::Context) -> Result<Vec<SinkTarget>> {
        let herdr = herdr::SystemHerdr::new(ctx.tools.herdr.as_deref())?;
        Ok(herdr::panes(&herdr)?
            .into_iter()
            .map(|p| SinkTarget {
                id: p.pane_id,
                name: p.agent,
                status: p.agent_status,
                cwd: p.cwd,
                title: p.terminal_title_stripped,
            })
            .collect())
    }

    fn preview(&self, ctx: &crate::context::Context, session: &Session, opts: &SubmitOptions) -> Result<SubmitPreview> {
        let options: HerdrOptions = opts.sink_options()?;
        let herdr = herdr::SystemHerdr::new(ctx.tools.herdr.as_deref())?;
        let view = crate::core::comments::narrowed(session, opts.comment_ids.as_deref())?;
        preview(
            view.as_ref().unwrap_or(session),
            &options,
            &herdr,
            &checkout_head_with(ctx),
        )
    }

    /// The lock is held across the send so a concurrent edit cannot slip
    /// into the queue between formatting and clearing it.
    fn submit(
        &self,
        ctx: &crate::context::Context,
        store: &crate::store::Store,
        id: &str,
        opts: &SubmitOptions,
    ) -> Result<(Session, SinkResult)> {
        let options: HerdrOptions = opts.sink_options()?;
        if opts.expect_digest.is_some() {
            return Err(crate::error::Error::invalid(
                "the herdr sink gives no preview digest to expect",
            ));
        }
        let herdr = herdr::SystemHerdr::new(ctx.tools.herdr.as_deref())?;
        let checkout_head = checkout_head_with(ctx);
        store.update(id, |s| {
            let view = crate::core::comments::narrowed(s, opts.comment_ids.as_deref())?;
            let Submitted { mut result, elsewhere } =
                submit(view.as_ref().unwrap_or(s), &options, &herdr, &checkout_head)?;
            if view.is_some() && s.decision.is_some() {
                result.decision = DecisionStatus::Skipped;
            }
            finish_summary(&mut result, elsewhere);
            let consume = self.consumes(&s.source, opts.consume);
            crate::core::apply::apply(s, result, consume)
        })
    }
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
    fn an_agent_in_a_subdirectory_or_behind_a_symlink_is_in_the_repository() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(repo.join("sub")).unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&repo, &link).unwrap();
        let sub = repo.join("sub").to_string_lossy().to_string();
        assert_eq!(
            resolve_target(&[agent("s", &sub, &sub)], &repo, false).unwrap(),
            "s",
            "started in a subdirectory"
        );
        let linked = link.to_string_lossy().to_string();
        assert_eq!(
            resolve_target(&[agent("l", &linked, &linked)], &repo, false).unwrap(),
            "l"
        );
        let root = repo.to_string_lossy().to_string();
        assert_eq!(
            resolve_target(&[agent("s", &sub, &sub), agent("r", &root, &root)], &repo, false).unwrap(),
            "r",
            "the agent at the root wins over one in a subdirectory"
        );
        let nested = repo.join(".claude/worktrees/feat");
        std::fs::create_dir_all(nested.join("src")).unwrap();
        std::fs::write(nested.join(".git"), "gitdir: /elsewhere\n").unwrap();
        for inside in [nested.clone(), nested.join("src")] {
            let inside = inside.to_string_lossy().to_string();
            assert_eq!(
                resolve_target(&[agent("n", &inside, &inside)], &repo, false)
                    .unwrap_err()
                    .code,
                ErrorCode::EAmbiguous,
                "another checkout nested under the root is not this repository"
            );
        }
        let sibling = dir.path().join("repo-other").to_string_lossy().to_string();
        assert_eq!(
            resolve_target(&[agent("o", &sibling, &sibling)], &repo, false)
                .unwrap_err()
                .code,
            ErrorCode::EAmbiguous,
            "a sibling whose name starts the same is not inside"
        );
    }

    #[test]
    fn claude_references_with_spaces_fall_back_to_a_quoted_path() {
        let mut c = session().comments[0].clone();
        c.path = "docs/my notes.md".into();
        assert_eq!(
            reference(&c, AgentType::Claude),
            format!("`docs/my notes.md`:{}", c.line)
        );
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
            .map(|c| c["id"].as_str().unwrap())
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
            ErrorCode::ETool
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
        s.comments[1].anchor.context.lines = vec!["let count = 1;".into()];
        s.decision = Some(Decision {
            event: DecisionEvent::RequestChanges,
            body: "Two things before merge.".into(),
            updated_at: at,
            deliveries: vec![],
        });
        s
    }

    #[test]
    fn prompt_format_for_claude() {
        let opts = HerdrOptions {
            header: Some("Review feedback:".into()),
            ..Default::default()
        };
        insta::assert_snapshot!(format_prompt(&session(), &opts, None));
    }

    #[test]
    fn suggestion_fence_grows_past_inner_backticks() {
        assert_eq!(fence_for("let x = 1;"), "```");
        assert_eq!(fence_for("```rust\nx\n```"), "````");
        assert_eq!(fence_for("`````"), "``````");
    }

    #[test]
    fn prompt_format_for_plain_agents_has_no_at_prefix() {
        let opts = HerdrOptions {
            agent_type: Some(AgentType::Plain),
            ..Default::default()
        };
        let prompt = format_prompt(&session(), &opts, None);
        assert!(prompt.contains("1. src/a.rs:3\n"));
        assert!(prompt.contains("2. src/b.rs:8-10 (nit)"));
        assert!(!prompt.contains('@'));
    }
}
