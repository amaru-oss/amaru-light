//! What a person can do to tickets, from the command line or the web page.

use crate::{
    app::parse_issue,
    lifecycle::{BLOCKED_LABEL, Services, unblock},
    settings::ProjectSettings,
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

/// Find the project and number of `owner/name#42`, a GitHub issue URL, or a
/// Jira key such as `CAMEL-12345` or its URL.
fn resolve<'a>(svc: &'a Services, reference: &str) -> Result<(&'a ProjectSettings, u64)> {
    let reference = reference.trim();
    if let Some(found) = svc.settings.jira_issue(reference) {
        return Ok(found);
    }
    let (repo, number) = parse_issue(reference)
        .context("expected an issue as owner/name#number, or a Jira key such as CAMEL-12345")?;
    let project = svc
        .settings
        .project(&repo)
        .with_context(|| format!("{repo} is not a configured project"))?;
    if let Some(jira) = &project.jira {
        bail!(
            "{repo} takes its tickets from Jira: claim {} instead",
            jira.key(number)
        );
    }
    Ok((project, number))
}

/// Queue an issue as a manual ticket: `owner/name#42`, its URL, or a Jira
/// key such as `CAMEL-12345`.
pub async fn claim(svc: &Services, reference: &str) -> Result<Claimed> {
    let (project, number) = resolve(svc, reference)?;
    let repo = project.repo.clone();
    let name = project.issue_name(number);
    if let Some(existing) = svc.store.open_ticket(&repo, number)? {
        return Ok(Claimed::Exists(existing));
    }
    let issue = svc.source.issue(&repo, number).await?;
    if !issue.open {
        bail!("{name} is closed");
    }
    match svc.store.claim_issue(&repo, &issue, Origin::Manual)? {
        Some(ticket) => Ok(Claimed::Queued(ticket)),
        None => {
            let existing = svc.store.open_ticket(&repo, number)?;
            Ok(Claimed::Exists(existing.context("ticket vanished")?))
        }
    }
}

/// Let a blocked ticket continue from the step where it stopped.
pub async fn resume(svc: &Services, id: i64) -> Result<Ticket> {
    let ticket = svc.store.get(id)?.context("no such ticket")?;
    if ticket.state != State::Blocked {
        bail!("ticket {id} is {}, not blocked", ticket.state);
    }
    reopen(svc, ticket).await
}

async fn reopen(svc: &Services, mut ticket: Ticket) -> Result<Ticket> {
    svc.source
        .remove_label(&ticket.repo, ticket.number, BLOCKED_LABEL)
        .await?;
    unblock(&mut ticket);
    svc.store.save(&ticket)?;
    svc.store.event(
        ticket.id,
        "UNBLOCKED",
        &serde_json::json!({ "to": ticket.state }),
    )?;
    Ok(ticket)
}

/// Longest guidance the daemon accepts, in bytes.
pub const MAX_GUIDANCE: usize = 20_000;

/// Give the ticket's agent directions in its own session. A blocked ticket
/// continues at once; a running agent is stopped and resumed with the
/// guidance; a ticket waiting for review runs a turn for it and pushes.
/// The daemon acts on it at its next poll.
pub async fn guide(svc: &Services, id: i64, text: &str, by: &str) -> Result<Ticket> {
    let text = text.trim();
    if text.is_empty() {
        bail!("write what the agent should do");
    }
    if text.len() > MAX_GUIDANCE {
        bail!("guidance is limited to {MAX_GUIDANCE} bytes");
    }
    let ticket = svc.store.get(id)?.context("no such ticket")?;
    if ticket.state.is_terminal() {
        bail!("ticket {id} is already {}", ticket.state);
    }
    let stopped = (ticket.state == State::Blocked)
        .then(|| ticket.reason.clone())
        .flatten();
    svc.store.add_guidance(id, by, text, stopped.as_deref())?;
    svc.store.event(
        id,
        "GUIDANCE",
        &serde_json::json!({ "by": by, "text": text, "state": ticket.state }),
    )?;
    if ticket.state == State::Blocked {
        return reopen(svc, ticket).await;
    }
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
    async fn claims_a_jira_key() {
        let fake = Fake::jira().await;
        fake.forge
            .update_issue(12, |i| i.key = Some("APP-12".into()));
        let Claimed::Queued(ticket) = claim(&fake.svc, "https://jira.example.com/browse/APP-12")
            .await
            .unwrap()
        else {
            unreachable!("a new issue is queued")
        };
        assert_eq!((ticket.repo.as_str(), ticket.number), ("me/app", 12));
        assert_eq!(ticket.reference(), "APP-12");
        assert!(matches!(
            claim(&fake.svc, "app-12").await.unwrap(),
            Claimed::Exists(_)
        ));
        let err = claim(&fake.svc, "me/app#12").await.unwrap_err();
        assert!(format!("{err:#}").contains("claim APP-12 instead"));
    }

    #[tokio::test]
    async fn refuses_a_closed_issue() {
        let fake = Fake::new("").await;
        fake.forge.update_issue(9, |i| i.open = false);
        assert!(claim(&fake.svc, "me/app#9").await.is_err());
    }
}
