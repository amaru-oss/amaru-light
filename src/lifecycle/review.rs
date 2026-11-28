//! Watching the pull request and acting on the feedback it receives.

use super::{BLOCKED_LABEL, Job, Services, WORKING_LABEL, blocked, try_label, turn::Session};
use crate::{
    forge::{Feedback, FeedbackKind, PullRequest},
    settings::ProjectSettings,
    ticket::{State, Ticket},
};
use anyhow::{Context, Result};
use serde_json::json;

/// Feedback the agent should act on: from an allowed reviewer or a failing
/// check, and not handled before.
fn actionable(
    project: &ProjectSettings,
    bot: Option<&str>,
    ticket: &Ticket,
    all: Vec<Feedback>,
) -> Vec<Feedback> {
    let review = &project.review;
    all.into_iter()
        .filter(|f| !ticket.handled_feedback.contains(&f.id))
        .filter(|f| bot.is_none_or(|b| !f.author.eq_ignore_ascii_case(b)))
        .filter(|f| match f.kind {
            FeedbackKind::FailedCheck => review.fix_ci,
            _ => {
                review.respond
                    && review
                        .reviewers
                        .iter()
                        .any(|r| r.eq_ignore_ascii_case(&f.author))
            }
        })
        .collect()
}

fn describe(feedback: &[Feedback]) -> String {
    feedback
        .iter()
        .map(|f| {
            let place = match (&f.path, f.line) {
                (Some(path), Some(line)) => format!(" on {path}:{line}"),
                (Some(path), None) => format!(" on {path}"),
                _ => String::new(),
            };
            format!(
                "- id `{}` ({:?} by {}{place}):\n  {}",
                f.id,
                f.kind,
                f.author,
                f.body.replace('\n', "\n  ")
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

async fn pending(
    svc: &Services,
    project: &ProjectSettings,
    ticket: &Ticket,
    pr: &PullRequest,
) -> Result<Vec<Feedback>> {
    let all = svc.forge.feedback(&ticket.repo, pr).await?;
    let bot = svc.settings.daemon.bot_login.as_deref();
    Ok(actionable(project, bot, ticket, all))
}

/// React to feedback the agent will act on, so the reviewer sees it is being
/// addressed. Best-effort: a token that may not react must not stop the
/// ticket. Each review or comment gets one attempt, whatever the outcome.
async fn acknowledge(
    svc: &Services,
    project: &ProjectSettings,
    ticket: &mut Ticket,
    feedback: &[Feedback],
) {
    let reaction = project.review.reaction;
    if reaction.github_name().is_none() {
        return;
    }
    for item in feedback {
        if item.kind == FeedbackKind::FailedCheck || ticket.reacted_feedback.contains(&item.id) {
            continue;
        }
        ticket.reacted_feedback.push(item.id.clone());
        if let Err(e) = svc.forge.react(item, reaction).await {
            let error = format!("{e:#}");
            crate::scheduler::log(&format!(
                "{}: cannot react to {}: {error}",
                ticket.reference(),
                item.id
            ));
            let payload = json!({ "feedback": item.id, "error": error });
            // Losing this event only loses the note; the ticket goes on.
            let _ = svc.store.event(ticket.id, "REACTION_SKIPPED", &payload);
        }
    }
}

async fn finish(svc: &Services, ticket: &mut Ticket) -> Result<()> {
    ticket.state = State::Done;
    svc.store.save(ticket)?;
    svc.store
        .event(ticket.id, "STATE", &json!({ "to": State::Done }))?;
    svc.source
        .remove_label(&ticket.repo, ticket.number, WORKING_LABEL)
        .await?;
    if let Some(worktree) = ticket.worktree.take() {
        // A worktree that is already gone needs no cleanup.
        let _ = svc.workspace.remove(&ticket.repo, &worktree).await;
        svc.store.save(ticket)?;
    }
    Ok(())
}

/// Check a ticket waiting for review: merged, closed, or new feedback.
pub async fn poll_review(svc: &Services, mut ticket: Ticket) -> Result<Ticket> {
    let project = svc
        .settings
        .project(&ticket.repo)
        .context("unknown project")?;
    let number = ticket.pr_number.context("ticket has no pull request")?;
    let pr = svc.forge.pr(&ticket.repo, number).await?;
    if pr.merged {
        finish(svc, &mut ticket).await?;
    } else if !pr.open {
        ticket.blocked_from = Some(State::AwaitingReview);
        ticket.reason = Some("the pull request was closed without merging".into());
        ticket.state = State::Blocked;
        try_label(svc, &mut ticket, BLOCKED_LABEL).await;
        svc.store.save(&ticket)?;
    } else {
        let feedback = pending(svc, project, &ticket, &pr).await?;
        if feedback.is_empty() {
            return Ok(ticket);
        }
        acknowledge(svc, project, &mut ticket, &feedback).await;
        start_addressing(svc, &mut ticket)?;
    }
    Ok(ticket)
}

fn start_addressing(svc: &Services, ticket: &mut Ticket) -> Result<()> {
    ticket.state = State::AddressingReview;
    svc.store.save(ticket)?;
    svc.store.event(
        ticket.id,
        "STATE",
        &json!({ "to": State::AddressingReview }),
    )
}

impl Job<'_> {
    /// Resume the session with the new feedback, verify, push and reply.
    pub(super) async fn address_review(&mut self) -> Result<State> {
        let number = self
            .ticket
            .pr_number
            .context("ticket has no pull request")?;
        let pr = self.svc.forge.pr(&self.ticket.repo, number).await?;
        let feedback = Box::pin(self.screened_feedback(&pr)).await?;
        if feedback.is_empty() {
            return Ok(State::AwaitingReview);
        }
        self.acknowledge(&feedback).await?;
        self.next_review_round()?;
        let prompt = self.svc.prompts.render(
            "address_review",
            &[("pr_url", &pr.url), ("feedback", &describe(&feedback))],
        );
        let report = self
            .turn(&self.project.agent.clone(), &prompt, Session::Ticket)
            .await?;
        let (replies, summary) = (report.replies.clone(), report.summary.clone());
        self.accept(report, State::AwaitingReview).await?;
        self.ticket.fix_rounds = 0;
        while self.check_until_green().await? {}
        self.commit_and_push().await?;
        self.reply(number, &feedback, &replies, &summary).await?;
        self.ticket
            .handled_feedback
            .extend(feedback.into_iter().map(|f| f.id));
        Ok(State::AwaitingReview)
    }

    /// The new feedback that the guard lets through.
    async fn screened_feedback(&mut self, pr: &PullRequest) -> Result<Vec<Feedback>> {
        let feedback = pending(self.svc, self.project, &self.ticket, pr).await?;
        self.screen_feedback(pr.number, feedback).await
    }

    /// React to feedback that arrived after the poll, and remember it.
    async fn acknowledge(&mut self, feedback: &[Feedback]) -> Result<()> {
        acknowledge(self.svc, self.project, &mut self.ticket, feedback).await;
        self.save()
    }

    fn next_review_round(&mut self) -> Result<()> {
        self.ticket.review_rounds += 1;
        let max = self.project.review.max_rounds;
        if self.ticket.review_rounds > max {
            return Err(blocked(format!(
                "review feedback is still open after {max} rounds"
            )));
        }
        Ok(())
    }

    /// One check-and-fix round; true while another round is needed.
    async fn check_until_green(&mut self) -> Result<bool> {
        match self.check().await? {
            State::Checking => Ok(true),
            _ => Ok(false),
        }
    }

    async fn commit_and_push(&self) -> Result<()> {
        let worktree = self.worktree()?;
        self.svc
            .workspace
            .commit_leftovers(&worktree, "Address review feedback")
            .await?;
        let branch = self.ticket.branch.clone().context("ticket has no branch")?;
        self.ensure_remotes().await?;
        self.svc
            .workspace
            .push(&worktree, &self.target(), &branch)
            .await
    }

    async fn reply(
        &self,
        pr: u64,
        feedback: &[Feedback],
        replies: &[crate::contract::Reply],
        summary: &str,
    ) -> Result<()> {
        let repo = &self.ticket.repo;
        for item in feedback {
            if let Some(reply) = replies.iter().find(|r| r.thread_id == item.id) {
                self.svc.forge.reply(repo, pr, item, &reply.body).await?;
            }
        }
        let note = format!("Amaru addressed the latest feedback:\n\n{summary}");
        self.svc.source.comment(repo, pr, &note).await
    }
}
