use super::*;
use crate::{lifecycle::BLOCKED_LABEL, testing::Fake, ticket::State};
use std::time::Duration;

fn scheduler(fake: &Fake, intake: Intake) -> Arc<Scheduler> {
    Arc::new(Scheduler::new(Arc::clone(&fake.svc), intake))
}

/// Run one poll and wait for the tickets it started.
async fn settle(scheduler: &Arc<Scheduler>) {
    scheduler.tick().await.unwrap();
    for _ in 0..200 {
        if lock(&scheduler.running).unwrap().is_empty() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        lock(&scheduler.running).unwrap().is_empty(),
        "tickets did not settle"
    );
}

fn states(fake: &Fake) -> Vec<(u64, State)> {
    let mut all: Vec<_> = fake
        .svc
        .store
        .recent(50)
        .unwrap()
        .iter()
        .map(|t| (t.number, t.state))
        .collect();
    all.sort_unstable_by_key(|(n, _)| *n);
    all
}

#[tokio::test]
async fn claims_ready_issues_and_skips_the_rest() {
    let fake = Fake::new("").await;
    fake.forge.update_issue(1, |_| {});
    fake.forge
        .update_issue(2, |i| i.labels.push("amaru:hold".into()));
    fake.forge.update_issue(3, |i| i.blocked_by = vec![9]);
    let scheduler = scheduler(&fake, Intake::Autonomous);
    settle(&scheduler).await;
    assert_eq!(states(&fake), [(1, State::AwaitingReview)]);
}

#[tokio::test]
async fn stacks_once_the_dependency_has_a_pull_request() {
    let fake = Fake::new("").await;
    fake.forge.update_issue(1, |_| {});
    fake.forge.update_issue(2, |i| i.blocked_by = vec![1]);
    let scheduler = scheduler(&fake, Intake::Autonomous);
    settle(&scheduler).await;
    settle(&scheduler).await;
    assert_eq!(
        states(&fake),
        [(1, State::AwaitingReview), (2, State::AwaitingReview)]
    );
    let second = fake.svc.store.open_ticket("me/app", 2).unwrap().unwrap();
    assert_eq!(second.stacked_on, Some(1));
}

#[tokio::test]
async fn removing_the_blocked_label_resumes_and_closing_cancels() {
    let fake = Fake::new("false").await;
    fake.forge.update_issue(1, |_| {});
    let scheduler = scheduler(&fake, Intake::Autonomous);
    settle(&scheduler).await;
    assert_eq!(states(&fake), [(1, State::Blocked)]);
    fake.forge
        .state()
        .labels
        .retain(|(_, l)| l != BLOCKED_LABEL);
    scheduler.tick().await.unwrap();
    let ticket = fake.svc.store.open_ticket("me/app", 1).unwrap().unwrap();
    assert_eq!(ticket.fix_rounds, 0);
    settle(&scheduler).await;
    fake.forge.update_issue(1, |i| i.open = false);
    settle(&scheduler).await;
    assert_eq!(states(&fake), [(1, State::Cancelled)]);
}

#[tokio::test]
async fn a_milestone_drain_stops_when_nothing_is_left() {
    let fake = Fake::new("").await;
    fake.forge
        .update_issue(1, |i| i.milestone = Some("M1".into()));
    fake.forge.update_issue(2, |_| {});
    let intake = Intake::Milestone {
        repo: "me/app".into(),
        milestone: "M1".into(),
    };
    let scheduler = scheduler(&fake, intake);
    let (_stop, stop_rx) = watch::channel(false);
    let run = tokio::time::timeout(Duration::from_secs(30), scheduler.run(stop_rx));
    run.await.unwrap().unwrap();
    assert_eq!(states(&fake), [(1, State::AwaitingReview)]);
}

#[tokio::test]
async fn a_blocked_ticket_without_its_label_waits_for_unblock() {
    let fake = Fake::new("false").await;
    fake.forge.state().denied.labels = true;
    fake.forge.update_issue(1, |_| {});
    let scheduler = scheduler(&fake, Intake::Autonomous);
    settle(&scheduler).await;
    assert_eq!(states(&fake), [(1, State::Blocked)]);
    settle(&scheduler).await;
    assert_eq!(states(&fake), [(1, State::Blocked)]);
    let mut ticket = fake.svc.store.open_ticket("me/app", 1).unwrap().unwrap();
    crate::lifecycle::unblock(&mut ticket);
    assert_eq!(ticket.state, State::Checking);
}
