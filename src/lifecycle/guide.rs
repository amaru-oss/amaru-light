//! Guidance from the operator: free-form directions that a ticket's agent
//! receives in its own session, to get it past a point where it is stuck.

use super::{Job, turn::Session, turn::as_refs};
use crate::{db::Guidance, ticket::State};
use anyhow::Result;
use serde_json::json;

/// Guidance from the operator, ready to place in a prompt.
fn directions(guidance: &[Guidance]) -> String {
    guidance
        .iter()
        .map(|g| g.text.trim())
        .collect::<Vec<_>>()
        .join("\n\n---\n\n")
}

impl Job<'_> {
    /// Guidance waiting for this ticket's agent.
    pub(super) fn pending_guidance(&self) -> Result<Vec<Guidance>> {
        self.svc.store.pending_guidance(self.ticket.id)
    }

    /// Whether the next step should be a guidance turn: there is guidance,
    /// and an agent session to give it to. Before the first turn the
    /// guidance goes into the implementation prompt instead.
    pub(super) fn wants_guidance_turn(&self) -> Result<bool> {
        let resumable = self.ticket.session_id.is_some()
            && matches!(
                self.ticket.state,
                State::Implementing
                    | State::Checking
                    | State::SelfReview
                    | State::Publishing
                    | State::AddressingReview
            );
        Ok(resumable && !self.pending_guidance()?.is_empty())
    }

    /// Mark the guidance as given to the agent, before its turn starts: newer
    /// guidance that arrives during the turn interrupts it, this one does not.
    fn deliver(&self, guidance: &[Guidance]) -> Result<()> {
        let ids: Vec<i64> = guidance.iter().map(|g| g.id).collect();
        self.svc.store.deliver_guidance(&ids)?;
        self.event(
            "GUIDANCE_DELIVERED",
            &json!({ "ids": ids, "state": self.ticket.state }),
        )
    }

    /// Append pending guidance to the first prompt of a ticket.
    pub(super) fn with_guidance(&self, prompt: String) -> Result<String> {
        let guidance = self.pending_guidance()?;
        if guidance.is_empty() {
            return Ok(prompt);
        }
        self.deliver(&guidance)?;
        Ok(format!(
            "{prompt}\n\nThe person who runs Amaru added these directions, which take precedence:\n\n{}\n",
            directions(&guidance)
        ))
    }

    /// What the ticket was doing when the guidance came.
    fn situation(&self, guidance: &[Guidance]) -> String {
        let doing = match self.ticket.state {
            State::Implementing => "You were implementing the issue.",
            State::Checking => {
                "Your changes were going through the project checks, and fixes for them."
            }
            State::SelfReview => "Your changes were about to be reviewed before publishing.",
            State::Publishing => "Your changes were about to be pushed for a pull request.",
            State::AddressingReview => "You were addressing review feedback on the pull request.",
            _ => "",
        };
        let stopped = guidance
            .iter()
            .rev()
            .find_map(|g| g.stopped.as_deref())
            .map(|r| {
                format!(
                    "\n\nAmaru had stopped the work because:\n\n> {}",
                    r.replace('\n', "\n> ")
                )
            })
            .unwrap_or_default();
        format!("{doing}{stopped}")
    }

    /// Resume the agent's session with the operator's guidance, then verify
    /// the result as after any other change: the checks run again.
    pub(super) async fn follow_guidance(&mut self) -> Result<State> {
        let guidance = self.pending_guidance()?;
        let mut vars = self.issue_vars();
        vars.push(("guidance".into(), directions(&guidance)));
        vars.push(("situation".into(), self.situation(&guidance)));
        let prompt = self.svc.prompts.render("guide", &as_refs(&vars));
        self.log("giving the agent the operator's guidance");
        self.deliver(&guidance)?;
        // The operator looked at it: the fix budget starts over.
        self.ticket.fix_rounds = 0;
        let agent = self.project.agent.clone();
        let report = self.turn(&agent, &prompt, Session::Ticket).await?;
        let next = self.accept(report, self.after_change()?).await?;
        self.commit_leftovers("Follow the operator's guidance")
            .await?;
        self.require_commits().await?;
        Ok(next)
    }
}
