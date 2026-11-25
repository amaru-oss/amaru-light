//! What a person can do to tickets, from the command line or the web page.

use crate::{
    app::parse_issue,
    lifecycle::{BLOCKED_LABEL, Services, unblock},
    ticket::{Origin, State, Ticket},
};
use anyhow::{Context, Result, bail};

/// The outcome of claiming an issue.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Claimed {
    /// A new ticket was queued.
    Queued(Ticket),
    /// The issue already has an unfinished ticket.
    Exists(Ticket),
}

/// Queue an issue, `owner/name#42`, as a manual ticket.
pub async fn claim(svc: &Services, reference: &str) -> Result<Claimed> {
    let (repo, number) =
        parse_issue(reference.trim()).context("expected an issue as owner/name#number")?;
    let project = svc
        .settings
        .project(&repo)
        .with_context(|| format!("{repo} is not a configured project"))?;
    let repo = project.repo.clone();
    if let Some(existing) = svc.store.open_ticket(&repo, number)? {
        return Ok(Claimed::Exists(existing));
    }
    let issue = svc.source.issue(&repo, number).await?;
    if !issue.open {
        bail!("{repo}#{number} is closed");
    }
    match svc
        .store
        .claim(&repo, number, &issue.title, Origin::Manual)?
    {
        Some(ticket) => Ok(Claimed::Queued(ticket)),
        None => {
            let existing = svc.store.open_ticket(&repo, number)?;
            Ok(Claimed::Exists(existing.context("ticket vanished")?))
        }
    }
}

/// Let a blocked ticket continue from the step where it stopped.
pub async fn resume(svc: &Services, id: i64) -> Result<Ticket> {
    let mut ticket = svc.store.get(id)?.context("no such ticket")?;
    if ticket.state != State::Blocked {
        bail!("ticket {id} is {}, not blocked", ticket.state);
    }
    svc.source
        .remove_label(&ticket.repo, ticket.number, BLOCKED_LABEL)
        .await?;
    unblock(&mut ticket);
    svc.store.save(&ticket)?;
    svc.store
        .event(id, "UNBLOCKED", &serde_json::json!({ "to": ticket.state }))?;
    Ok(ticket)
}

/// Stop working on a ticket; a running daemon stops its agent at once.
pub fn cancel(svc: &Services, id: i64) -> Result<Ticket> {
    let mut ticket = svc.store.get(id)?.context("no such ticket")?;
    if ticket.state.is_terminal() {
        bail!("ticket {id} is already {}", ticket.state);
    }
    ticket.state = State::Cancelled;
    svc.store.save(&ticket)?;
    svc.store
        .event(id, "CANCELLED", &serde_json::json!({ "by": "operator" }))?;
    Ok(ticket)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::Fake;

    #[tokio::test]
    async fn claims_resumes_and_cancels() {
        let fake = Fake::new("").await;
        let Claimed::Queued(ticket) = claim(&fake.svc, " me/app#5 ").await.unwrap() else {
            unreachable!("a new issue is queued")
        };
        assert_eq!(ticket.number, 5);
        assert!(matches!(
            claim(&fake.svc, "me/app#5").await.unwrap(),
            Claimed::Exists(_)
        ));
        assert!(claim(&fake.svc, "other/app#5").await.is_err());
        assert!(claim(&fake.svc, "me/app").await.is_err());
        assert!(resume(&fake.svc, ticket.id).await.is_err());
        let mut blocked = ticket.clone();
        blocked.state = State::Blocked;
        blocked.blocked_from = Some(State::Implementing);
        fake.svc.store.save(&blocked).unwrap();
        assert_eq!(
            resume(&fake.svc, ticket.id).await.unwrap().state,
            State::Implementing
        );
        assert_eq!(
            cancel(&fake.svc, ticket.id).unwrap().state,
            State::Cancelled
        );
        assert!(cancel(&fake.svc, ticket.id).is_err());
    }

    #[tokio::test]
    async fn refuses_a_closed_issue() {
        let fake = Fake::new("").await;
        fake.forge.update_issue(9, |i| i.open = false);
        assert!(claim(&fake.svc, "me/app#9").await.is_err());
    }
}
