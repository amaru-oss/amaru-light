//! Claiming new issues from the configured intake.

use super::{Intake, Scheduler, has_label, log, rank};
use crate::{
    forge::{Issue, Selector},
    lifecycle::{BLOCKED_LABEL, WORKING_LABEL},
    settings::ProjectSettings,
    ticket::Origin,
};
use anyhow::Result;

impl Scheduler {
    pub(super) async fn claim_new(&self) -> Result<()> {
        let projects = self.svc.settings.projects.clone();
        for project in &projects {
            if !self.can_work(project) {
                continue;
            }
            let found = match &self.intake {
                Intake::Autonomous => self.autonomous(project).await,
                Intake::Milestone { repo, milestone }
                    if repo.eq_ignore_ascii_case(&project.repo) =>
                {
                    self.svc.source.milestone(repo, milestone).await
                }
                Intake::Milestone { .. } | Intake::Manual => continue,
            };
            match found {
                Ok(issues) => self.claim_ready(project, issues)?,
                Err(e) => log(&format!("{}: cannot list issues: {e:#}", project.repo)),
            }
        }
        Ok(())
    }

    async fn autonomous(&self, project: &ProjectSettings) -> Result<Vec<Issue>> {
        let mut labels = project.intake.labels.clone();
        labels.push(project.intake.analyze_label.clone());
        let selector = Selector {
            labels,
            assignee: project.intake.assignee.clone(),
        };
        self.svc.source.eligible(&project.repo, &selector).await
    }

    /// Claim issues that can start now, best ranked first.
    fn claim_ready(&self, project: &ProjectSettings, mut issues: Vec<Issue>) -> Result<()> {
        issues.sort_by_key(|i| rank(i, &project.intake.priority_labels));
        let origin = match self.intake {
            Intake::Milestone { .. } => Origin::Milestone,
            _ => Origin::Autonomous,
        };
        for issue in issues {
            if self.can_start(project, &issue)?
                && let Some(ticket) = self.svc.store.claim_issue(&project.repo, &issue, origin)?
            {
                log(&format!("claimed {}", ticket.reference()));
            }
        }
        Ok(())
    }

    /// Not skipped, not taken, and every dependency already has a pull request
    /// this issue can stack on.
    fn can_start(&self, project: &ProjectSettings, issue: &Issue) -> Result<bool> {
        let store = &self.svc.store;
        if !issue.open || store.open_ticket(&project.repo, issue.number)?.is_some() {
            return Ok(false);
        }
        // A Jira issue often stays open after its pull request is merged,
        // until someone resolves it; it must not be worked on again.
        if project.jira.is_some() && store.was_done(&project.repo, issue.number)? {
            return Ok(false);
        }
        let skipped = project
            .intake
            .skip_labels
            .iter()
            .any(|l| has_label(issue, l));
        if skipped || has_label(issue, WORKING_LABEL) || has_label(issue, BLOCKED_LABEL) {
            return Ok(false);
        }
        for dependency in &issue.blocked_by {
            let parent = store.open_ticket(&project.repo, *dependency)?;
            if parent.and_then(|p| p.pr_number).is_none() {
                return Ok(false);
            }
        }
        Ok(true)
    }
}
