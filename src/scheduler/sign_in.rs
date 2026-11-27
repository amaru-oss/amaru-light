//! Taking work only for agents that are signed in.
//!
//! A signed-out agent CLI fails every turn, so a ticket claimed for it only
//! collects errors until it is blocked. Each poll asks every agent the
//! projects use whether it is signed in; while one is not, no issue is
//! claimed and no ticket is started for the projects that use it.

use super::{Scheduler, log};
use crate::{login, settings::ProjectSettings};
use std::collections::BTreeSet;

impl Scheduler {
    /// Check the sign-in of every agent a project uses.
    pub(super) async fn check_agents(&self) {
        let settings = &self.svc.settings;
        let names: BTreeSet<&str> = settings
            .projects
            .iter()
            .flat_map(ProjectSettings::agents)
            .collect();
        for name in names {
            self.check_agent(name).await;
        }
    }

    /// Check one agent again; true when it can take work.
    async fn check_agent(&self, name: &str) -> bool {
        // Loading the settings already checked that every agent is defined.
        let Ok(agent) = self.svc.settings.agent(name) else {
            return true;
        };
        let found = login::readiness(agent).await;
        let ready = found.ready;
        let detail = found.detail.lines().next().unwrap_or_default().to_owned();
        if self.svc.sign_ins.record(name, found) {
            if ready {
                log(&format!(
                    "agent {name} is signed in; taking work for it again"
                ));
            } else {
                log(&format!(
                    "agent {name} is not signed in, so no work is claimed or started for it \
                     until it is: {detail}"
                ));
            }
        }
        ready
    }

    /// Whether every agent of `project` could take work at the last check.
    pub(super) fn can_work(&self, project: &ProjectSettings) -> bool {
        project
            .agents()
            .into_iter()
            .all(|name| self.svc.sign_ins.ready(name))
    }

    /// Check the agents of the ticket's project again, after a failed run.
    pub(super) async fn still_signed_in(&self, repo: &str) -> bool {
        let Some(project) = self.svc.settings.project(repo) else {
            return true;
        };
        let mut ready = true;
        for name in project.agents() {
            ready &= self.check_agent(name).await;
        }
        ready
    }
}
