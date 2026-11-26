//! Running tickets on the worker slots.

use super::{MAX_ERRORS, Scheduler, lock, log};
use crate::{lifecycle::Job, ticket::Ticket};
use anyhow::Result;
use std::sync::Arc;
use tokio::sync::watch;

impl Scheduler {
    /// Start every ticket that needs a slot and is not already running.
    pub(super) fn dispatch(self: &Arc<Self>) -> Result<()> {
        for ticket in self.svc.store.unfinished()? {
            if !ticket.state.needs_slot() || self.is_running(ticket.id)? {
                continue;
            }
            let (stop, cancel) = watch::channel(false);
            lock(&self.running)?.insert(ticket.id, stop);
            let me = Arc::clone(self);
            tokio::spawn(async move { Box::pin(me.work(ticket, cancel)).await });
        }
        Ok(())
    }

    async fn work(self: Arc<Self>, ticket: Ticket, cancel: watch::Receiver<bool>) {
        let id = ticket.id;
        let reference = ticket.reference();
        let slot = match Arc::clone(&self.slots).try_acquire_owned() {
            Ok(slot) => Ok(slot),
            Err(_) => {
                log(&format!(
                    "{reference}: waiting for a worker slot (all {} are busy)",
                    self.svc.settings.daemon.max_parallel
                ));
                Arc::clone(&self.slots).acquire_owned().await
            }
        };
        if let Ok(_slot) = slot {
            // The ticket may have waited for the slot for a long time; work
            // on it as it is now, not as it was when it was dispatched.
            match self.svc.store.get(id) {
                Ok(Some(current)) if current.state.needs_slot() => {
                    Box::pin(self.run_ticket(current, cancel)).await;
                }
                Ok(_) => {}
                Err(e) => log(&format!("{reference}: {e:#}")),
            }
        }
        if let Ok(mut running) = lock(&self.running) {
            running.remove(&id);
        }
    }

    async fn run_ticket(&self, ticket: Ticket, cancel: watch::Receiver<bool>) {
        let id = ticket.id;
        let reference = ticket.reference();
        log(&format!("{reference}: working ({})", ticket.state));
        match self.drive(ticket, cancel).await {
            Ok(state) => {
                log(&format!("{reference}: {state}"));
                if let Ok(mut errors) = lock(&self.errors) {
                    errors.remove(&id);
                }
            }
            Err(e) => log(&format!("{reference}: {e:#}")),
        }
    }

    /// Drive the ticket; after repeated errors, block it with the last one.
    async fn drive(
        &self,
        ticket: Ticket,
        cancel: watch::Receiver<bool>,
    ) -> Result<crate::ticket::State> {
        let id = ticket.id;
        let mut job = Job::new(&self.svc, ticket, Some(cancel))?;
        let Err(error) = job.drive().await else {
            return Ok(job.into_ticket().state);
        };
        let count = {
            let mut errors = lock(&self.errors)?;
            let count = errors.entry(id).or_default();
            *count += 1;
            *count
        };
        self.svc.store.event(
            id,
            "ERROR",
            &serde_json::json!({ "error": format!("{error:#}"), "count": count }),
        )?;
        if count >= MAX_ERRORS {
            // Start from the stored ticket: the job may have saved progress.
            let stored = self.svc.store.get(id)?.unwrap_or_else(|| job.into_ticket());
            Job::new(&self.svc, stored, None)?.fail(&error).await?;
            lock(&self.errors)?.remove(&id);
        }
        Err(error)
    }
}
