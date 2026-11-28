//! The ticket lifecycle: from a claimed issue to a pull request that a
//! human merges, with review feedback handled in the same agent session.

mod guide;
mod review;
mod screen;
mod steps;
mod turn;

use crate::{
    db::{Store, now},
    forge::{Forge, ForkError, Target, TicketSource},
    login::SignIns,
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
    /// Whether each agent is signed in, as the scheduler last checked.
    pub sign_ins: SignIns,
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

    /// Make sure the project's fork exists, creating it when it is missing.
    /// The daemon decides this, never the agent: a fork it cannot use stops
    /// the ticket with the reason.
    async fn ensure_fork(&self) -> Result<()> {
        let target = self.target();
        match self.svc.forge.ensure_fork(&target).await {
            Ok(()) => Ok(()),
            Err(e) if e.is::<ForkError>() => Err(blocked(format!("{e:#}"))),
            Err(e) => {
                let fork = target.fork.unwrap_or_default();
                Err(e.context(format!("cannot make sure the fork {fork} exists")))
            }
        }
    }

    /// Make sure the fork exists and the clone and the ticket's worktree have
    /// `origin` = where branches are pushed and `upstream` = the repository
    /// pull requests go to. Runs before every agent turn and every push, so
    /// whatever the agent did to the remotes, the work goes to the same place.
    async fn ensure_remotes(&self) -> Result<()> {
        self.ensure_fork().await?;
        let target = self.target();
        let worktree = self.ticket.worktree.as_deref();
        self.svc
            .workspace
            .ensure_remotes(&target, worktree)
            .await
            .map_err(|e| {
                blocked(format!(
                    "cannot set the git remotes of {}: {e:#}",
                    target.upstream
                ))
            })
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

    /// Write a progress line to the daemon log.
    fn log(&self, message: &str) {
        crate::scheduler::log(&format!("{}: {message}", self.ticket.reference()));
    }

    fn transition(&mut self, next: State) -> Result<()> {
        let from = self.ticket.state;
        if from != next {
            self.log(&format!("{from} -> {next}"));
        }
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
                return self.halt();
            }
            if let Err(e) = self.ensure_worktree().await {
                return self.stop(e).await;
            }
            let result = if self.wants_guidance_turn()? {
                Box::pin(self.follow_guidance()).await
            } else {
                Box::pin(self.step()).await
            };
            match result {
                Ok(next) => self.transition(next)?,
                Err(_) if self.is_cancelled() => return self.halt(),
                Err(e) => self.stop(e).await?,
            }
        }
        Ok(())
    }

    /// The job was told to stop. Either the ticket was cancelled, or new
    /// guidance came for its agent: then the job ends where it is, and the
    /// scheduler starts it again on its next poll with the guidance first.
    fn halt(&mut self) -> Result<()> {
        let stored = self.svc.store.get(self.ticket.id)?;
        let cancelled = stored.is_none_or(|t| t.state == State::Cancelled);
        if !cancelled && !self.pending_guidance()?.is_empty() {
            self.log("stopped the agent to give it the operator's guidance");
            return self.event("INTERRUPTED", &json!({ "for": "guidance" }));
        }
        self.transition(State::Cancelled)
    }

    /// Recreate a missing worktree (after cleanup or on a new machine).
    async fn ensure_worktree(&mut self) -> Result<()> {
        let needs = !matches!(self.ticket.state, State::Queued | State::Preparing);
        let present = self.ticket.worktree.as_ref().is_some_and(|p| p.exists());
        if needs && !present {
            self.create_worktree().await?;
            self.save()?;
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

    /// Block the ticket because of an error the daemon could not recover from.
    pub async fn fail(&mut self, error: &anyhow::Error) -> Result<()> {
        self.block(&format!("{error:#}")).await
    }

    async fn block(&mut self, reason: &str) -> Result<()> {
        self.ticket.blocked_from = Some(self.ticket.state);
        self.ticket.blocked_at = Some(now());
        self.ticket.reason = Some(reason.to_owned());
        let labelled = try_label(self.svc, &mut self.ticket, BLOCKED_LABEL).await;
        self.transition(State::Blocked)?;
        let (repo, number) = (&self.ticket.repo, self.ticket.number);
        let next = if labelled {
            format!("Remove the `{BLOCKED_LABEL}` label to let it continue.")
        } else {
            format!("Run `amaru unblock {}` to let it continue.", self.ticket.id)
        };
        let body = format!("Amaru stopped working on this issue:\n\n> {reason}\n\n{next}");
        self.svc.source.comment(repo, number, &body).await?;
        self.svc
            .source
            .remove_label(repo, number, WORKING_LABEL)
            .await
    }
}

/// Add a label to the ticket's issue and remember it; true when it is on the
/// issue.
///
/// Labelling needs write access to the repository, which a bot that
/// contributes through a fork often lacks. Without it the ticket goes on and
/// the failure is logged and recorded as an event.
pub async fn try_label(svc: &Services, ticket: &mut Ticket, label: &str) -> bool {
    match svc
        .source
        .add_label(&ticket.repo, ticket.number, label)
        .await
    {
        Ok(()) => {
            if !ticket.labels.iter().any(|l| l == label) {
                ticket.labels.push(label.to_owned());
            }
            true
        }
        Err(e) => {
            let error = format!("{e:#}");
            crate::scheduler::log(&format!(
                "{}: cannot add the {label} label: {error}",
                ticket.reference()
            ));
            let payload = json!({ "label": label, "error": error });
            // Losing this event only loses the note; the ticket goes on.
            let _ = svc.store.event(ticket.id, "LABEL_SKIPPED", &payload);
            false
        }
    }
}

/// Resume a blocked ticket where it stopped, with fresh fix budgets.
pub fn unblock(ticket: &mut Ticket) {
    ticket.state = ticket.blocked_from.take().unwrap_or(State::Queued);
    ticket.reason = None;
    ticket.blocked_at = None;
    ticket.labels.retain(|l| l != BLOCKED_LABEL);
    ticket.fix_rounds = 0;
    // A human looked at the ticket: the text the guard stopped may pass.
    if let Some(fingerprint) = ticket.guard_rejected.take()
        && !ticket.guard_allowed.contains(&fingerprint)
    {
        ticket.guard_allowed.push(fingerprint);
    }
    if ticket.state == State::AddressingReview {
        ticket.review_rounds = 0;
    }
}

#[cfg(test)]
mod tests;
