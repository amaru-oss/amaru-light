//! End-to-end lifecycle tests: a fake forge, a fake agent CLI and real git.

use super::*;
use crate::testing::Fake;
use crate::{
    forge::{Feedback, FeedbackKind},
    ticket::Origin,
};
use std::path::Path;

async fn drive(fake: &Fake, ticket: Ticket) -> Ticket {
    let mut job = Job::new(&fake.svc, ticket, None).unwrap();
    job.drive().await.unwrap();
    job.into_ticket()
}

fn file(worktree: &Path, name: &str) -> String {
    std::fs::read_to_string(worktree.join(name)).unwrap_or_default()
}

#[tokio::test]
async fn takes_an_issue_to_a_pull_request_in_one_session() {
    let fake = Fake::new("").await;
    let ticket = fake.claim(1).await;
    let ticket = drive(&fake, ticket).await;
    assert_eq!(ticket.state, State::AwaitingReview);
    assert_eq!(ticket.session_id.as_deref(), Some("sess-1"));
    assert_eq!(ticket.pr_number, Some(100));
    let worktree = ticket.worktree.clone().unwrap();
    assert_eq!(file(&worktree, "feature.txt"), "done\n");
    assert!(fake.upstream_has("amaru/1-add-a-feature").await);
    let forge = fake.forge.state();
    assert_eq!(forge.prs[0].base, "main");
    assert!(forge.prs[0].body.starts_with("Closes #1"));
    assert!(forge.labels.contains(&(1, WORKING_LABEL.to_owned())));
}

#[tokio::test]
async fn names_a_jira_ticket_by_its_key() {
    let fake = Fake::jira().await;
    fake.forge.update_issue(1, |i| i.key = Some("APP-1".into()));
    let ticket = drive(&fake, fake.claim(1).await).await;
    assert_eq!(ticket.state, State::AwaitingReview);
    assert_eq!(ticket.reference(), "APP-1");
    assert!(fake.upstream_has("amaru/APP-1-add-a-feature").await);
    let prompts = fake.prompts();
    assert!(prompts[0].contains("issue APP-1 of me/app"));
    assert!(prompts[0].contains("commit message with `APP-1: `"));
    let forge = fake.forge.state();
    assert_eq!(forge.prs[0].title, "APP-1: Add a feature");
    assert!(
        forge.prs[0]
            .body
            .starts_with("Fixes [APP-1](https://jira.example.com/browse/APP-1)")
    );
}

#[tokio::test]
async fn fixes_failing_checks_in_the_same_session() {
    let fake = Fake::new("test -f fixed.txt").await;
    let ticket = drive(&fake, fake.claim(1).await).await;
    assert_eq!(ticket.state, State::AwaitingReview);
    assert_eq!(ticket.fix_rounds, 1);
    let prompts = fake.prompts();
    assert!(prompts[1].contains("test -f fixed.txt"));
    assert!(fake.resumed_every_turn_after_the_first());
}

#[tokio::test]
async fn blocks_when_checks_keep_failing() {
    let fake = Fake::new("false").await;
    let ticket = drive(&fake, fake.claim(1).await).await;
    assert_eq!(ticket.state, State::Blocked);
    assert_eq!(ticket.blocked_from, Some(State::Checking));
    assert!(
        ticket
            .reason
            .unwrap()
            .contains("still fails after 3 fix rounds")
    );
    let forge = fake.forge.state();
    assert!(forge.labels.contains(&(1, BLOCKED_LABEL.to_owned())));
}

#[tokio::test]
async fn addresses_review_feedback_from_allowed_reviewers_only() {
    let fake = Fake::new("").await;
    let ticket = drive(&fake, fake.claim(1).await).await;
    fake.forge.add_feedback(feedback("line-7", "me"));
    fake.forge.add_feedback(feedback("line-8", "stranger"));
    let ticket = poll_review(&fake.svc, ticket).await.unwrap();
    assert_eq!(ticket.state, State::AddressingReview);
    let ticket = drive(&fake, ticket).await;
    assert_eq!(ticket.state, State::AwaitingReview);
    assert_eq!(ticket.handled_feedback, ["line-7"]);
    let replies = fake.forge.state().replies.clone();
    assert_eq!(replies, [("line-7".to_owned(), "renamed".to_owned())]);
    assert!(!fake.prompts().last().unwrap().contains("stranger"));
    let ticket = poll_review(&fake.svc, ticket).await.unwrap();
    assert_eq!(ticket.state, State::AwaitingReview);
}

#[tokio::test]
async fn reacts_once_to_each_comment_it_picks_up() {
    let fake = Fake::new("").await;
    let ticket = drive(&fake, fake.claim(1).await).await;
    fake.forge.add_feedback(feedback("line-7", "me"));
    fake.forge.add_feedback(feedback("line-8", "stranger"));
    let ticket = poll_review(&fake.svc, ticket).await.unwrap();
    assert_eq!(fake.forge.state().reactions, ["line-7"]);
    // A comment that lands before the agent starts is reacted to as well.
    fake.forge.add_feedback(feedback("comment-9", "me"));
    let ticket = drive(&fake, ticket).await;
    assert_eq!(ticket.handled_feedback, ["line-7", "comment-9"]);
    let ticket = poll_review(&fake.svc, ticket).await.unwrap();
    assert_eq!(ticket.state, State::AwaitingReview);
    assert_eq!(fake.forge.state().reactions, ["line-7", "comment-9"]);
    assert_eq!(ticket.reacted_feedback, ["line-7", "comment-9"]);
}

#[tokio::test]
async fn goes_on_when_reactions_are_refused() {
    let fake = Fake::new("").await;
    let ticket = drive(&fake, fake.claim(1).await).await;
    fake.forge.state().denied.reactions = true;
    fake.forge.add_feedback(feedback("line-7", "me"));
    let ticket = poll_review(&fake.svc, ticket).await.unwrap();
    assert_eq!(ticket.state, State::AddressingReview);
    let ticket = drive(&fake, ticket).await;
    assert_eq!(ticket.state, State::AwaitingReview);
    assert_eq!(ticket.handled_feedback, ["line-7"]);
    assert_eq!(ticket.reacted_feedback, ["line-7"]);
    let events = fake.svc.store.events(ticket.id, 0).unwrap();
    assert!(events.iter().any(|e| e.kind == "REACTION_SKIPPED"));
}

#[tokio::test]
async fn finishes_when_the_pull_request_is_merged() {
    let fake = Fake::new("").await;
    let ticket = drive(&fake, fake.claim(1).await).await;
    let worktree = ticket.worktree.clone().unwrap();
    fake.forge.merge();
    let ticket = poll_review(&fake.svc, ticket).await.unwrap();
    assert_eq!(ticket.state, State::Done);
    assert!(!worktree.exists());
}

#[tokio::test]
async fn stacks_on_an_unmerged_dependency() {
    let fake = Fake::new("").await;
    let first = drive(&fake, fake.claim(1).await).await;
    fake.forge.set_blocked_by(2, vec![1]);
    let second = drive(&fake, fake.claim(2).await).await;
    assert_eq!(second.stacked_on, Some(1));
    assert_eq!(second.base, first.branch);
    let forge = fake.forge.state();
    assert_eq!(forge.prs[1].base, "amaru/1-add-a-feature");
}

fn feedback(id: &str, author: &str) -> Feedback {
    Feedback {
        id: id.into(),
        kind: FeedbackKind::LineComment,
        author: author.into(),
        body: "please rename".into(),
        path: Some("feature.txt".into()),
        line: Some(1),
        node_id: Some(format!("node-{id}")),
    }
}

#[tokio::test]
async fn unblock_returns_to_the_blocked_state() {
    let mut ticket = Ticket::new(1, "me/app".into(), 1, "t".into(), Origin::Manual);
    ticket.state = State::Blocked;
    ticket.blocked_from = Some(State::Checking);
    ticket.fix_rounds = 4;
    unblock(&mut ticket);
    assert_eq!(ticket.state, State::Checking);
    assert_eq!(ticket.fix_rounds, 0);
}

#[tokio::test]
async fn works_without_permission_to_label_issues() {
    let fake = Fake::new("false").await;
    fake.forge.state().denied.labels = true;
    let ticket = drive(&fake, fake.claim(1).await).await;
    assert_eq!(ticket.state, State::Blocked);
    assert!(ticket.labels.is_empty());
    let forge = fake.forge.state();
    assert!(forge.labels.is_empty());
    let (_, last) = forge.comments.last().unwrap();
    assert!(last.contains(&format!("amaru unblock {}", ticket.id)));
    let events = fake.svc.store.events(ticket.id, 0).unwrap();
    assert!(events.iter().any(|e| e.kind == "LABEL_SKIPPED"));
}
