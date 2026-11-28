//! Analyzing an issue instead of implementing it: one agent turn, whose
//! analysis is posted on the issue.

use super::{Job, blocked, review::release, turn::Session, turn::as_refs};
use crate::ticket::State;
use anyhow::Result;

/// Where the agent writes its analysis, relative to the worktree.
pub const ANALYSIS_FILE: &str = ".amaru/analysis.md";

impl Job<'_> {
    /// Run the analysis, comment with it on the issue, and finish.
    pub(super) async fn analyze(&mut self) -> Result<State> {
        let worktree = self.worktree()?;
        let file = worktree.join(ANALYSIS_FILE);
        if file.exists() {
            std::fs::remove_file(&file)?;
        }
        let prompt = self
            .svc
            .prompts
            .render("analyze", &as_refs(&self.issue_vars()));
        let prompt = self.with_guidance(prompt)?;
        let agent = self.project.agent.clone();
        let report = self.turn(&agent, &prompt, Session::Ticket).await?;
        self.accept(report, State::Done).await?;
        let analysis = std::fs::read_to_string(&file).unwrap_or_default();
        if analysis.trim().is_empty() {
            return Err(blocked(format!(
                "the agent finished without writing {ANALYSIS_FILE}"
            )));
        }
        let body = format!("Amaru's analysis of this issue:\n\n{}", analysis.trim());
        let (repo, number) = (self.ticket.repo.clone(), self.ticket.number);
        self.svc.source.comment(&repo, number, &body).await?;
        release(self.svc, &mut self.ticket).await?;
        Ok(State::Done)
    }
}
