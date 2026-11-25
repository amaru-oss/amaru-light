//! The steps from a claimed issue to an open pull request.

use super::{Job, WORKING_LABEL, blocked, try_label, turn::Session, turn::as_refs};
use crate::{
    checks,
    contract::{Report, Status},
    forge::{NewPullRequest, PullRequest, Target},
    settings::SelfReview,
    ticket::{State, slug},
};
use anyhow::{Context, Result};

impl Job<'_> {
    /// Read the issue, pick the branch and its base, and announce the work.
    pub(super) async fn prepare(&mut self) -> Result<State> {
        let (repo, number) = (self.ticket.repo.clone(), self.ticket.number);
        let issue = self.svc.source.issue(&repo, number).await?;
        if !issue.open {
            return Ok(State::Cancelled);
        }
        self.ticket.title = issue.title;
        self.ticket.body = issue.body;
        self.ticket.key = issue.key;
        self.ticket.branch = Some(format!(
            "{}{}-{}",
            self.project.branch_prefix,
            self.ticket.label(),
            slug(&self.ticket.title)
        ));
        self.choose_base(&issue.blocked_by)?;
        try_label(self.svc, &mut self.ticket, WORKING_LABEL).await;
        let branch = self.ticket.branch.clone().unwrap_or_default();
        let note = format!("Amaru is working on this issue on branch `{branch}`.");
        self.svc.source.comment(&repo, number, &note).await?;
        Ok(State::Preparing)
    }

    /// Start from the base branch, or stack on the ticket this one depends on.
    fn choose_base(&mut self, blocked_by: &[u64]) -> Result<()> {
        self.ticket.base = Some(self.project.base_branch.clone());
        self.ticket.stacked_on = None;
        for &dependency in blocked_by {
            let parent = self.svc.store.open_ticket(&self.ticket.repo, dependency)?;
            match parent.and_then(|p| p.branch.filter(|_| p.pr_number.is_some())) {
                Some(branch) if self.ticket.stacked_on.is_none() => {
                    self.ticket.base = Some(branch);
                    self.ticket.stacked_on = Some(dependency);
                }
                Some(_) => {}
                None => {
                    return Err(blocked(format!(
                        "waiting for {}, which this issue depends on",
                        self.project.issue_name(dependency)
                    )));
                }
            }
        }
        Ok(())
    }

    pub(super) async fn create_worktree(&mut self) -> Result<State> {
        let target = self.target();
        self.log(&format!("syncing {}", target.upstream));
        self.svc.workspace.sync(&target).await?;
        let branch = self.ticket.branch.clone().context("ticket has no branch")?;
        let path = self
            .svc
            .workspace
            .create(&target, &branch, &self.base_ref()?)
            .await?;
        self.ticket.worktree = Some(path);
        Ok(State::Implementing)
    }

    /// Record the agent's report and turn its status into the next state.
    pub(super) async fn accept(&mut self, report: Report, next: State) -> Result<State> {
        self.ticket.summary = Some(report.summary.clone());
        self.ticket.complete = report.complete;
        self.save()?;
        match report.status {
            Status::Done => Ok(next),
            Status::Blocked => Err(blocked(report.summary)),
            Status::NeedsClarification => {
                let question = report.question.unwrap_or(report.summary);
                let body =
                    format!("Amaru needs clarification before it can continue:\n\n{question}");
                let (repo, number) = (self.ticket.repo.clone(), self.ticket.number);
                self.svc.source.comment(&repo, number, &body).await?;
                Err(blocked(format!("waiting for an answer: {question}")))
            }
        }
    }

    pub(super) async fn implement(&mut self) -> Result<State> {
        let resumed = self.ticket.session_id.is_some() && self.ticket.summary.is_some();
        if resumed {
            self.refresh_jira_body().await?;
        }
        let vars = self.issue_vars();
        let name = if resumed { "continue" } else { "implement" };
        let mut vars = vars;
        vars.push((
            "reason".into(),
            self.ticket.summary.clone().unwrap_or_default(),
        ));
        let prompt = self.svc.prompts.render(name, &as_refs(&vars));
        let report = self
            .turn(&self.project.agent.clone(), &prompt, Session::Ticket)
            .await?;
        let next = self.accept(report, State::Checking).await?;
        self.commit_leftovers("Complete the agent's changes")
            .await?;
        if self
            .svc
            .workspace
            .ahead(&self.worktree()?, &self.base_ref()?)
            .await?
            == 0
        {
            return Err(blocked("the agent finished without committing any change"));
        }
        Ok(next)
    }

    /// The agent cannot read Jira itself: give it the latest comments.
    async fn refresh_jira_body(&mut self) -> Result<()> {
        if self.ticket.key.is_some() {
            let (repo, number) = (self.ticket.repo.clone(), self.ticket.number);
            self.ticket.body = self.svc.source.issue(&repo, number).await?.body;
        }
        Ok(())
    }

    async fn commit_leftovers(&self, message: &str) -> Result<()> {
        let worktree = self.worktree()?;
        self.svc
            .workspace
            .commit_leftovers(&worktree, message)
            .await?;
        Ok(())
    }

    /// The state after the checks pass.
    fn after_checks(&self) -> State {
        let review = self.project.review.self_review != SelfReview::Off;
        if review && !self.ticket.self_reviewed {
            State::SelfReview
        } else {
            State::Publishing
        }
    }

    /// Count a fix round, blocking once the budget is spent.
    fn next_fix_round(&mut self, failure: &checks::Failure) -> Result<()> {
        self.ticket.fix_rounds += 1;
        let max = self.project.max_fix_rounds;
        if self.ticket.fix_rounds > max {
            return Err(blocked(format!(
                "`{}` still fails after {max} fix rounds:\n\n{}",
                failure.command, failure.output
            )));
        }
        Ok(())
    }

    /// Run the checks; on failure, resume the session to fix them.
    pub(super) async fn check(&mut self) -> Result<State> {
        let worktree = self.worktree()?;
        let cancel = self.cancel.clone();
        self.log("running the checks");
        let Some(failure) = checks::run(&self.project.checks, &worktree, cancel).await? else {
            self.log("checks passed");
            return Ok(self.after_checks());
        };
        self.log(&format!(
            "check `{}` failed ({})",
            failure.name, failure.exit
        ));
        self.event("CHECK_FAILED", &serde_json::to_value(&failure)?)?;
        self.next_fix_round(&failure)?;
        let vars = [
            ("check", failure.command.as_str()),
            ("output", failure.output.as_str()),
        ];
        let prompt = self.svc.prompts.render("fix_checks", &vars);
        let agent = self.project.agent.clone();
        let report = self.turn(&agent, &prompt, Session::Ticket).await?;
        let next = self.accept(report, State::Checking).await?;
        self.commit_leftovers("Fix the project checks").await?;
        Ok(next)
    }

    /// An independent reviewer session looks at the change before it is published.
    pub(super) async fn self_review(&mut self) -> Result<State> {
        let reviewer = match self.project.review.self_review {
            SelfReview::OtherAgent => self.project.review.reviewer_agent.clone(),
            _ => None,
        };
        let reviewer = reviewer.unwrap_or_else(|| self.project.agent.clone());
        let prompt = self
            .svc
            .prompts
            .render("self_review", &as_refs(&self.issue_vars()));
        let review = self.turn(&reviewer, &prompt, Session::Separate).await?;
        self.ticket.self_reviewed = true;
        self.event("SELF_REVIEW", &serde_json::json!({ "status": format!("{:?}", review.status), "summary": review.summary }))?;
        if review.status == Status::Done {
            return Ok(State::Publishing);
        }
        let prompt = self
            .svc
            .prompts
            .render("address_self_review", &[("findings", &review.summary)]);
        let report = self
            .turn(&self.project.agent.clone(), &prompt, Session::Ticket)
            .await?;
        let next = self.accept(report, State::Checking).await?;
        self.commit_leftovers("Address the review findings").await?;
        Ok(next)
    }

    /// Push the branch and open (or find) the pull request.
    pub(super) async fn publish(&mut self) -> Result<State> {
        self.commit_leftovers("Complete the agent's changes")
            .await?;
        let target = self.target();
        let branch = self.ticket.branch.clone().context("ticket has no branch")?;
        self.svc
            .workspace
            .push(&self.worktree()?, &target, &branch)
            .await?;
        let pr = self.ensure_pr(&target, &branch).await?;
        self.ticket.pr_number = Some(pr.number);
        self.save()?;
        let note = format!("Amaru opened {} for this issue.", pr.url);
        let (repo, number) = (&self.ticket.repo, self.ticket.number);
        self.svc.source.comment(repo, number, &note).await?;
        Ok(State::AwaitingReview)
    }

    async fn ensure_pr(&self, target: &Target, branch: &str) -> Result<PullRequest> {
        let existing = self.svc.forge.find_pr(target, branch).await?;
        match existing.filter(|pr| pr.open) {
            Some(pr) => Ok(pr),
            None => {
                self.svc
                    .forge
                    .open_pr(target, &self.new_pr(branch).await?)
                    .await
            }
        }
    }

    async fn new_pr(&self, branch: &str) -> Result<NewPullRequest> {
        let t = &self.ticket;
        // A pull request from a fork cannot target another fork branch, so a
        // stacked fork ticket targets the base branch and says so.
        let stacked_base = t.stacked_on.is_some() && self.project.fork.is_none();
        let base = if stacked_base { t.base.clone() } else { None };
        let worktree = self.worktree()?;
        let (log, _) = self
            .svc
            .workspace
            .summary(&worktree, &self.base_ref()?)
            .await?;
        Ok(NewPullRequest {
            branch: branch.to_owned(),
            base: base.unwrap_or_else(|| self.project.base_branch.clone()),
            title: self.pr_title(),
            body: self.pr_body(&log),
        })
    }

    /// The issue title; a Jira ticket's starts with its key, as projects such
    /// as Apache Camel require.
    fn pr_title(&self) -> String {
        let t = &self.ticket;
        match &t.key {
            Some(key) if !t.title.starts_with(key.as_str()) => format!("{key}: {}", t.title),
            _ => t.title.clone(),
        }
    }

    /// How the pull request names its issue: `#42`, or a link to Jira.
    fn issue_link(&self, number: u64) -> String {
        match &self.project.jira {
            Some(jira) => format!("[{}]({})", jira.key(number), jira.browse_url(number)),
            None => format!("#{number}"),
        }
    }

    fn pr_body(&self, log: &str) -> String {
        let t = &self.ticket;
        let jira = self.project.jira.is_some();
        let link = match (t.complete, jira) {
            (true, false) => "Closes",
            (true, true) => "Fixes",
            (false, _) => "Part of",
        };
        let stacked = t
            .stacked_on
            .map(|n| {
                format!(
                    "\n\nThis builds on the work for {}, which is not merged yet.",
                    self.issue_link(n)
                )
            })
            .unwrap_or_default();
        let checks: Vec<String> = self
            .project
            .checks
            .iter()
            .map(|c| format!("- `{}` passed", c.run))
            .collect();
        let summary = t.summary.clone().unwrap_or_default();
        format!(
            "{link} {}{stacked}\n\n## Summary\n{summary}\n\n## Commits\n{log}\n## Verification\n{}\n\n_Opened by Amaru Light._",
            self.issue_link(t.number),
            checks.join("\n"),
        )
    }
}
