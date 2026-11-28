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
async fn analyzes_a_labelled_issue_and_comments() {
    let fake = Fake::new("false").await;
    fake.forge
        .update_issue(1, |i| i.labels.push("amaru:analyze".into()));
    let ticket = drive(&fake, fake.claim(1).await).await;
    assert_eq!(ticket.goal, crate::ticket::Goal::Analyze);
    assert_eq!(ticket.state, State::Done);
    assert!(ticket.worktree.is_none());
    assert_eq!(ticket.pr_number, None);
    assert!(fake.prompts()[0].contains("Do not implement it."));
    assert!(!fake.upstream_has("amaru/1-add-a-feature").await);
    let forge = fake.forge.state();
    assert!(forge.prs.is_empty());
    let last = &forge.comments.last().unwrap().1;
    assert!(last.contains("Root cause: the parser."), "{last}");
    assert!(forge.comments[0].1.contains("analyzing this issue"));
    assert!(!forge.labels.contains(&(1, WORKING_LABEL.to_owned())));
}

#[tokio::test]
async fn blocks_an_analysis_without_a_file() {
    let fake = Fake::new("").await;
    std::fs::write(fake.dir.path().join("agent/no-analysis"), "").unwrap();
    let mut ticket = fake.claim(1).await;
    ticket.goal = crate::ticket::Goal::Analyze;
    let ticket = drive(&fake, ticket).await;
    assert_eq!(ticket.state, State::Blocked);
    assert_eq!(ticket.blocked_from, Some(State::Analyzing));
    assert!(ticket.reason.unwrap().contains(".amaru/analysis.md"));
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

#[tokio::test]
async fn a_silent_agent_is_stopped_and_retried_in_its_session() {
    let fake = Fake::with_project("", "idle_timeout: 1s\n").await;
    std::fs::write(fake.dir.path().join("agent/stall"), "").unwrap();
    let ticket = fake.claim(1).await;
    let mut job = Job::new(&fake.svc, ticket, None).unwrap();
    let error = job.drive().await.unwrap_err();
    assert!(format!("{error:#}").contains("made no progress for 1s"));
    let id = job.into_ticket().id;
    let stored = fake.svc.store.get(id).unwrap().unwrap();
    assert_eq!(stored.state, State::Implementing);
    assert_eq!(stored.session_id.as_deref(), Some("sess-1"));
    let ticket = drive(&fake, stored).await;
    assert_eq!(ticket.state, State::AwaitingReview);
    assert!(fake.resumed_every_turn_after_the_first());
}

const FORK: &str = "fork: bot/app\n";

#[tokio::test]
async fn creates_the_fork_and_pushes_only_there() {
    let fake = Fake::with_project("", FORK).await;
    // The agent points the remotes elsewhere; the daemon puts them back.
    std::fs::write(fake.dir.path().join("agent/tamper"), "").unwrap();
    let ticket = drive(&fake, fake.claim(1).await).await;
    assert_eq!(ticket.state, State::AwaitingReview);
    let branch = "amaru/1-add-a-feature";
    assert!(fake.repo_has("bot/app", branch).await);
    assert!(!fake.upstream_has(branch).await);
    let forge = fake.forge.state();
    assert_eq!(forge.forks_created, ["bot/app"]);
    assert_eq!(forge.heads, [format!("bot:{branch}")]);
    // Before the worktree, before the agent's turn and before the push.
    assert!(forge.fork_checks >= 3, "{}", forge.fork_checks);
}

#[tokio::test]
async fn a_jira_ticket_goes_through_the_fork_too() {
    let extra = format!("{FORK}jira: {{url: \"https://jira.example.com\", project: APP}}\n");
    let fake = Fake::with_project("", &extra).await;
    fake.forge.update_issue(1, |i| i.key = Some("APP-1".into()));
    let ticket = drive(&fake, fake.claim(1).await).await;
    assert_eq!(ticket.state, State::AwaitingReview);
    let branch = "amaru/APP-1-add-a-feature";
    assert!(fake.repo_has("bot/app", branch).await);
    assert!(!fake.upstream_has(branch).await);
    let forge = fake.forge.state();
    assert_eq!(forge.forks_created, ["bot/app"]);
    assert_eq!(forge.heads, [format!("bot:{branch}")]);
}

#[tokio::test]
async fn blocks_before_any_agent_turn_when_the_fork_is_unusable() {
    let fake = Fake::with_project("", FORK).await;
    fake.forge.state().fork_unusable = true;
    let ticket = drive(&fake, fake.claim(1).await).await;
    assert_eq!(ticket.state, State::Blocked);
    assert!(ticket.reason.unwrap_or_default().contains("not a fork"));
    assert!(fake.prompts().is_empty());
    assert!(!fake.repo_has("bot/app", "amaru/1-add-a-feature").await);
}

#[tokio::test]
async fn guidance_resumes_a_blocked_ticket_in_its_session() {
    let fake = Fake::new("test -f guided.txt").await;
    let ticket = drive(&fake, fake.claim(1).await).await;
    assert_eq!(ticket.state, State::Blocked);
    let guided = crate::actions::guide(&fake.svc, ticket.id, "Add guided.txt", "web")
        .await
        .unwrap();
    assert_eq!(guided.state, State::Checking);
    let ticket = drive(&fake, guided).await;
    assert_eq!(ticket.state, State::AwaitingReview);
    let prompts = fake.prompts();
    let turn = prompts
        .iter()
        .find(|p| p.contains("Add guided.txt"))
        .unwrap();
    assert!(turn.contains("stepped in on your work"));
    assert!(turn.contains("still fails after 3 fix rounds"));
    assert!(fake.resumed_every_turn_after_the_first());
    assert!(
        fake.svc
            .store
            .pending_guidance(ticket.id)
            .unwrap()
            .is_empty()
    );
    assert_eq!(fake.forge.state().prs.len(), 1);
}

#[tokio::test]
async fn guidance_before_the_first_turn_goes_into_its_prompt() {
    let fake = Fake::new("").await;
    let ticket = fake.claim(1).await;
    crate::actions::guide(&fake.svc, ticket.id, "Keep it small", "command line")
        .await
        .unwrap();
    let ticket = drive(&fake, ticket).await;
    assert_eq!(ticket.state, State::AwaitingReview);
    let prompts = fake.prompts();
    assert!(prompts[0].contains("You are working on issue"));
    assert!(prompts[0].contains("Keep it small"));
    assert!(
        fake.svc
            .store
            .pending_guidance(ticket.id)
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn refuses_empty_guidance_and_finished_tickets() {
    let fake = Fake::new("").await;
    let ticket = fake.claim(1).await;
    assert!(
        crate::actions::guide(&fake.svc, ticket.id, " \n", "web")
            .await
            .is_err()
    );
    crate::actions::cancel(&fake.svc, ticket.id).unwrap();
    assert!(
        crate::actions::guide(&fake.svc, ticket.id, "go", "web")
            .await
            .is_err()
    );
    assert!(
        crate::actions::guide(&fake.svc, 99, "go", "web")
            .await
            .is_err()
    );
}
