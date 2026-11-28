//! Keeping unfinished tickets in step with GitHub and with the operator.

use super::{Scheduler, has_label, lock, log};
use crate::{
    lifecycle::{BLOCKED_LABEL, poll_review, state_after_change, unblock},
    settings::Retention,
    ticket::{State, Ticket},
};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};

impl Scheduler {
    /// Cancel, unblock, clean up, or poll the pull request of one ticket.
    pub(super) async fn maintain(&self, mut ticket: Ticket) -> Result<()> {
        if self.is_running(ticket.id)? {
            return self.forward_stop(&ticket);
        }
        let issue = self.svc.source.issue(&ticket.repo, ticket.number).await?;
        if !issue.open {
            ticket.state = State::Cancelled;
            log(&format!("{} was closed; cancelled", ticket.reference()));
            return self.svc.store.save(&ticket);
        }
        match ticket.state {
            State::Blocked
                if ticket.labels.iter().any(|l| l == BLOCKED_LABEL)
                    && !has_label(&issue, BLOCKED_LABEL) =>
            {
                unblock(&mut ticket);
                log(&format!("{} unblocked", ticket.reference()));
                self.svc.store.save(&ticket)
            }
            State::Blocked => self.clean_up(ticket).await,
            State::AwaitingReview if self.has_guidance(&ticket)? => self.take_guidance(ticket),
            State::AwaitingReview => poll_review(&self.svc, ticket).await.map(|_| ()),
            _ => Ok(()),
        }
    }

    fn has_guidance(&self, ticket: &Ticket) -> Result<bool> {
        Ok(!self.svc.store.pending_guidance(ticket.id)?.is_empty())
    }

    /// Wake a ticket that waits for review because the operator has guidance
    /// for its agent: it runs the guidance turn and the checks, and pushes.
    fn take_guidance(&self, mut ticket: Ticket) -> Result<()> {
        ticket.state = state_after_change(&self.svc, ticket.clone())?;
        log(&format!("{}: guidance for the agent", ticket.reference()));
        self.svc.store.save(&ticket)?;
        let payload = serde_json::json!({ "from": State::AwaitingReview, "to": ticket.state });
        self.svc.store.event(ticket.id, "STATE", &payload)
    }

    /// Stop a running ticket that was cancelled, or whose agent has new
    /// guidance waiting: the scheduler then starts it again, guidance first.
    fn forward_stop(&self, ticket: &Ticket) -> Result<()> {
        let stored = self.svc.store.get(ticket.id)?.context("ticket vanished")?;
        // Before the first agent turn there is nothing to interrupt: the
        // guidance goes into that turn's prompt.
        let early = matches!(stored.state, State::Queued | State::Preparing);
        let guided = !early && self.has_guidance(ticket)?;
        if (stored.state == State::Cancelled || guided)
            && let Some(stop) = lock(&self.running)?.get(&ticket.id)
        {
            // The job may have finished in the meantime; nothing to stop then.
            let _ = stop.send(true);
        }
        Ok(())
    }

    /// Remove the worktree of a ticket blocked for longer than the retention.
    async fn clean_up(&self, mut ticket: Ticket) -> Result<()> {
        let project = self
            .svc
            .settings
            .project(&ticket.repo)
            .context("unknown project")?;
        let Retention::For(keep) = project.workspace.keep_blocked else {
            return Ok(());
        };
        let Some(since) = ticket.blocked_at.as_deref() else {
            return Ok(());
        };
        let since: DateTime<Utc> = since.parse()?;
        let expired = Utc::now()
            .signed_duration_since(since)
            .to_std()
            .unwrap_or_default()
            >= keep;
        if let (true, Some(worktree)) = (expired, ticket.worktree.take()) {
            self.svc.workspace.remove(&ticket.repo, &worktree).await?;
            self.svc.store.save(&ticket)?;
            log(&format!(
                "{}: removed the blocked worktree",
                ticket.reference()
            ));
        }
        Ok(())
    }
}
