//! The daemon loop: claim work, keep tickets moving, watch pull requests.

use crate::{db::now, forge::Issue, lifecycle::Services};
use anyhow::Result;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
use tokio::sync::{Semaphore, watch};

/// Errors in a row after which a ticket is blocked instead of retried.
const MAX_ERRORS: u32 = 3;

/// Where new work comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Intake {
    /// Issues matching each project's label and assignee selectors.
    Autonomous,
    /// Every open issue of one milestone, until none is left to start.
    Milestone { repo: String, milestone: String },
    /// Only tickets claimed by hand.
    Manual,
}

pub fn log(message: &str) {
    eprintln!("{} {message}", now());
}

/// Keeps tickets moving with at most `max_parallel` running at once.
pub struct Scheduler {
    svc: Arc<Services>,
    slots: Arc<Semaphore>,
    running: Arc<Mutex<HashMap<i64, watch::Sender<bool>>>>,
    errors: Arc<Mutex<HashMap<i64, u32>>>,
    intake: Intake,
}

impl std::fmt::Debug for Scheduler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Scheduler")
            .field("intake", &self.intake)
            .finish_non_exhaustive()
    }
}

fn has_label(issue: &Issue, label: &str) -> bool {
    issue.labels.iter().any(|l| l.eq_ignore_ascii_case(label))
}

/// Sort key: priority label first, then milestone due date, then age.
fn rank(issue: &Issue, priority: &[String]) -> (usize, String, String) {
    let level = priority
        .iter()
        .position(|p| has_label(issue, p))
        .unwrap_or(priority.len());
    let due = issue.milestone_due.clone().unwrap_or_else(|| "~".into());
    (level, due, issue.created_at.clone())
}

fn lock<T>(mutex: &Mutex<T>) -> Result<std::sync::MutexGuard<'_, T>> {
    mutex
        .lock()
        .map_err(|_| anyhow::anyhow!("scheduler lock poisoned"))
}

impl Scheduler {
    pub fn new(svc: Arc<Services>, intake: Intake) -> Self {
        let slots = svc.settings.daemon.max_parallel;
        Self {
            svc,
            slots: Arc::new(Semaphore::new(slots)),
            running: Arc::default(),
            errors: Arc::default(),
            intake,
        }
    }

    /// Poll until `stop` is set, or until a milestone drain has nothing left.
    pub async fn run(self: Arc<Self>, mut stop: watch::Receiver<bool>) -> Result<()> {
        let interval = self.svc.settings.daemon.poll_interval.0;
        loop {
            if let Err(e) = self.tick().await {
                log(&format!("poll failed: {e:#}"));
            }
            if self.intake != Intake::Autonomous && self.drained()? {
                log("done: every ticket has a pull request or needs a human");
                return Ok(());
            }
            tokio::select! {
                () = tokio::time::sleep(interval) => {}
                _ = stop.wait_for(|s| *s) => return Ok(()),
            }
        }
    }

    /// One round: claim new work, update tickets, start what can run.
    pub async fn tick(self: &Arc<Self>) -> Result<()> {
        self.claim_new().await?;
        for ticket in self.svc.store.unfinished()? {
            if let Err(e) = self.maintain(ticket).await {
                log(&format!("ticket update failed: {e:#}"));
            }
        }
        self.dispatch()
    }

    fn is_running(&self, id: i64) -> Result<bool> {
        Ok(lock(&self.running)?.contains_key(&id))
    }

    fn drained(&self) -> Result<bool> {
        let busy = self
            .svc
            .store
            .unfinished()?
            .iter()
            .any(|t| t.state.needs_slot());
        Ok(!busy && lock(&self.running)?.is_empty())
    }
}

mod intake;
mod maintain;
mod run;

#[cfg(test)]
mod tests;
