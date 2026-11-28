//! Untrusted text goes through the prompt injection guard before an agent
//! reads it. Rejected text never reaches the agent.

use super::{Job, blocked};
use crate::{
    forge::Feedback,
    guard::{Guard, Verdict, fingerprint},
    settings::{GuardInput, GuardOnError},
};
use anyhow::Result;
use serde_json::json;

/// Text the guard did not let through.
pub(super) struct Rejection {
    /// Why, for the human: what was checked, the label and the score.
    pub reason: String,
    pub fingerprint: String,
}

impl Job<'_> {
    /// Check `text`, an untrusted `input` described as `what`, with the guard.
    /// `None` means the text may reach the agent: the guard passed it, a
    /// human let it through, or the guard does not check this input.
    pub(super) async fn screen(
        &self,
        input: GuardInput,
        what: &str,
        text: &str,
    ) -> Result<Option<Rejection>> {
        if !self.svc.settings.daemon.guard.checks(input) || text.trim().is_empty() {
            return Ok(None);
        }
        let print = fingerprint(text);
        if self.ticket.guard_allowed.contains(&print) {
            let payload = json!({ "input": input, "what": what, "by": "human" });
            self.event("GUARD_ALLOWED", &payload)?;
            return Ok(None);
        }
        let Some(guard) = Guard::of(&self.svc.settings) else {
            return Ok(None);
        };
        let verdict = Box::pin(guard.check(text)).await;
        self.judge_verdict(input, what, verdict).map(|why| {
            why.map(|reason| Rejection {
                reason,
                fingerprint: print,
            })
        })
    }

    /// Record the guard's answer; the reason for the human when it rejected
    /// the text. A guard that gave no answer stops the ticket, unless
    /// `on_error` is `allow`.
    fn judge_verdict(
        &self,
        input: GuardInput,
        what: &str,
        verdict: Result<Verdict>,
    ) -> Result<Option<String>> {
        match verdict {
            Ok(Verdict::Pass) => {
                self.event("GUARD_PASSED", &json!({ "input": input, "what": what }))?;
                Ok(None)
            }
            Ok(Verdict::Reject { why }) => {
                self.log(&format!("the guard rejected {what} ({why})"));
                let payload = json!({ "input": input, "what": what, "why": why });
                self.event("GUARD_REJECTED", &payload)?;
                Ok(Some(format!(
                    "the prompt guard flagged {what} as a possible prompt injection ({why}), \
                     so it was not given to the agent"
                )))
            }
            Err(e) => {
                let error = format!("{e:#}");
                self.log(&format!("the guard could not check {what}: {error}"));
                let payload = json!({ "input": input, "what": what, "error": error });
                self.event("GUARD_ERROR", &payload)?;
                match self.svc.settings.daemon.guard.on_error {
                    GuardOnError::Allow => Ok(None),
                    GuardOnError::Block => Err(blocked(format!(
                        "the prompt guard could not check {what}: {error}"
                    ))),
                }
            }
        }
    }

    /// Check `text` and stop the ticket when the guard rejects it. A human
    /// who unblocks the ticket lets this exact text through.
    pub(super) async fn screen_or_block(
        &mut self,
        input: GuardInput,
        what: &str,
        text: &str,
    ) -> Result<()> {
        match Box::pin(self.screen(input, what, text)).await? {
            None => Ok(()),
            Some(rejection) => {
                self.ticket.guard_rejected = Some(rejection.fingerprint);
                self.save()?;
                Err(blocked(rejection.reason))
            }
        }
    }

    /// Check the issue's title and body, as the agent would read them.
    pub(super) async fn screen_issue(&mut self) -> Result<()> {
        if !self.svc.settings.daemon.guard.checks(GuardInput::Issue) {
            return Ok(());
        }
        if !self.ticket.guard_allowed.is_empty() {
            // A human unblocked the ticket after the guard stopped it, maybe
            // after editing the issue: check what the issue says now.
            let (repo, number) = (self.ticket.repo.clone(), self.ticket.number);
            let issue = self.svc.source.issue(&repo, number).await?;
            self.ticket.title = issue.title;
            self.ticket.body = issue.body;
            self.save()?;
        }
        let text = format!("{}\n\n{}", self.ticket.title, self.ticket.body);
        self.screen_or_block(GuardInput::Issue, "the issue", &text)
            .await
    }

    /// Keep the feedback the guard lets through. Each rejected item is
    /// marked handled, so it is never given to the agent, and gets a reply.
    pub(super) async fn screen_feedback(
        &mut self,
        pr: u64,
        feedback: Vec<Feedback>,
    ) -> Result<Vec<Feedback>> {
        let mut kept = Vec::with_capacity(feedback.len());
        for item in feedback {
            let what = format!("{:?} `{}` by {}", item.kind, item.id, item.author);
            let Some(rejection) =
                Box::pin(self.screen(GuardInput::Review, &what, &item.body)).await?
            else {
                kept.push(item);
                continue;
            };
            self.ticket.handled_feedback.push(item.id.clone());
            self.save()?;
            let note = format!(
                "Amaru did not act on this feedback: {}. \
                 A maintainer can restate it in a new comment.",
                rejection.reason
            );
            let repo = self.ticket.repo.clone();
            self.svc.forge.reply(&repo, pr, &item, &note).await?;
        }
        Ok(kept)
    }
}
