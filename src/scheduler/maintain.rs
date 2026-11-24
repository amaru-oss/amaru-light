//! Keeping unfinished tickets in step with GitHub and with the operator.

use super::{Scheduler, has_label, lock, log};
use crate::{
    lifecycle::{BLOCKED_LABEL, poll_review, unblock},
    settings::Retention,
    ticket::{State, Ticket},
};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};

impl Scheduler {
    /// Cancel, unblock, clean up, or poll the pull request of one ticket.
    pub(super) async fn maintain(&self, mut ticket: Ticket) -> Result<()> {
        if self.is_running(ticket.id)? {
            return self.forward_cancel(&ticket);
        }
        let issue = self.svc.source.issue(&ticket.repo, ticket.number).await?;
        if !issue.open {
            ticket.state = State::Cancelled;
            log(&format!("{} was closed; cancelled", ticket.reference()));
            return self.svc.store.save(&ticket);
        }
        match ticket.state {
            State::Blocked if !has_label(&issue, BLOCKED_LABEL) => {
                unblock(&mut ticket);
                log(&format!("{} unblocked", ticket.reference()));
                self.svc.store.save(&ticket)
            }
            State::Blocked => self.clean_up(ticket).await,
            State::AwaitingReview => poll_review(&self.svc, ticket).await.map(|_| ()),
            _ => Ok(()),
        }
    }

    /// Stop a running ticket that was cancelled from the command line.
    fn forward_cancel(&self, ticket: &Ticket) -> Result<()> {
        let stored = self.svc.store.get(ticket.id)?.context("ticket vanished")?;
        if stored.state == State::Cancelled
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
