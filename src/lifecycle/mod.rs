//! The ticket lifecycle: from a claimed issue to a pull request that a
//! human merges, with review feedback handled in the same agent session.

mod review;
mod steps;
mod turn;

use crate::{
    db::{Store, now},
    forge::{Forge, Target, TicketSource},
    process::Cancel,
    prompts::Prompts,
    settings::{ProjectSettings, Settings},
    ticket::{State, Ticket},
    worktree::Workspace,
};
use anyhow::{Context, Result};
use serde_json::json;
use std::{fmt, path::PathBuf, sync::Arc};

pub use review::poll_review;

/// Marks an issue the daemon is working on.
pub const WORKING_LABEL: &str = "amaru:working";
/// Marks an issue the daemon stopped on; removing it resumes the ticket.
pub const BLOCKED_LABEL: &str = "amaru:blocked";

/// Everything a ticket needs to make progress.
pub struct Services {
    pub settings: Arc<Settings>,
    pub store: Arc<Store>,
    pub source: Arc<dyn TicketSource>,
    pub forge: Arc<dyn Forge>,
    pub workspace: Workspace,
    pub prompts: Prompts,
}

impl fmt::Debug for Services {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Services").finish_non_exhaustive()
    }
}

/// A step stopped the ticket for a reason a human has to look at.
#[derive(Debug)]
pub struct Blocked(pub String);

impl fmt::Display for Blocked {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Blocked {}

/// The ticket was cancelled while a step ran.
#[derive(Debug)]
pub struct Cancelled;

impl fmt::Display for Cancelled {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("cancelled")
    }
}

impl std::error::Error for Cancelled {}

/// Stop the ticket with a reason for the human.
pub fn blocked(reason: impl Into<String>) -> anyhow::Error {
    Blocked(reason.into()).into()
}

/// One ticket being moved through its states.
pub struct Job<'a> {
    svc: &'a Services,
    project: &'a ProjectSettings,
    ticket: Ticket,
    cancel: Option<Cancel>,
}

impl<'a> Job<'a> {
    pub fn new(svc: &'a Services, ticket: Ticket, cancel: Option<Cancel>) -> Result<Self> {
        let project = svc
            .settings
            .project(&ticket.repo)
            .with_context(|| format!("{} is not a configured project", ticket.repo))?;
        Ok(Self {
            svc,
            project,
            ticket,
            cancel,
        })
    }

    pub fn into_ticket(self) -> Ticket {
        self.ticket
    }

    fn target(&self) -> Target {
        Target {
            upstream: self.project.repo.clone(),
            fork: self.project.fork.clone(),
        }
    }

    fn worktree(&self) -> Result<PathBuf> {
        self.ticket
            .worktree
            .clone()
            .context("ticket has no worktree")
    }

    fn base_ref(&self) -> Result<String> {
        self.ticket.base_ref().context("ticket has no base")
    }

    fn save(&self) -> Result<()> {
        self.svc.store.save(&self.ticket)
    }

    fn event(&self, kind: &str, payload: &serde_json::Value) -> Result<()> {
        self.svc.store.event(self.ticket.id, kind, payload)
    }

    fn transition(&mut self, next: State) -> Result<()> {
        let from = self.ticket.state;
        self.ticket.state = next;
        self.save()?;
        self.event("STATE", &json!({ "from": from, "to": next }))
    }

    fn is_cancelled(&self) -> bool {
        self.cancel.as_ref().is_some_and(|c| *c.borrow())
    }

    /// Run steps until the ticket waits for a review, a human, or is done.
    pub async fn drive(&mut self) -> Result<()> {
        while self.ticket.state.needs_slot() {
            if self.is_cancelled() {
                return self.transition(State::Cancelled);
            }
            match self.step().await {
                Ok(next) => self.transition(next)?,
                Err(e) => self.stop(e).await?,
            }
        }
        Ok(())
    }

    async fn step(&mut self) -> Result<State> {
        match self.ticket.state {
            State::Queued => self.prepare().await,
            State::Preparing => self.create_worktree().await,
            State::Implementing => self.implement().await,
            State::Checking => self.check().await,
            State::SelfReview => self.self_review().await,
            State::Publishing => self.publish().await,
            State::AddressingReview => self.address_review().await,
            other => Ok(other),
        }
    }

    /// Turn a step error into a blocked or cancelled ticket; pass others on.
    async fn stop(&mut self, error: anyhow::Error) -> Result<()> {
        if error.is::<Cancelled>() {
            return self.transition(State::Cancelled);
        }
        match error.downcast::<Blocked>() {
            Ok(Blocked(reason)) => self.block(&reason).await,
            Err(other) => Err(other),
        }
    }

    async fn block(&mut self, reason: &str) -> Result<()> {
        self.ticket.blocked_from = Some(self.ticket.state);
        self.ticket.blocked_at = Some(now());
        self.ticket.reason = Some(reason.to_owned());
        self.transition(State::Blocked)?;
        let (repo, number) = (&self.ticket.repo, self.ticket.number);
        let body = format!(
            "Amaru stopped working on this issue:\n\n> {reason}\n\nRemove the `{BLOCKED_LABEL}` label to let it continue."
        );
        self.svc.source.comment(repo, number, &body).await?;
        self.svc
            .source
            .add_label(repo, number, BLOCKED_LABEL)
            .await?;
        self.svc
            .source
            .remove_label(repo, number, WORKING_LABEL)
            .await
    }
}

/// Resume a blocked ticket where it stopped, with fresh fix budgets.
pub fn unblock(ticket: &mut Ticket) {
    ticket.state = ticket.blocked_from.take().unwrap_or(State::Queued);
    ticket.reason = None;
    ticket.blocked_at = None;
    ticket.fix_rounds = 0;
    if ticket.state == State::AddressingReview {
        ticket.review_rounds = 0;
    }
}

#[cfg(test)]
mod tests;
