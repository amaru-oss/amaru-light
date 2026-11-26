//! GitHub through the `gh` CLI, authenticated by the `GH_TOKEN` of the bot.

use super::{
    Feedback, FeedbackKind, Forge, ForkError, Issue, NewPullRequest, PullRequest, Reaction,
    Selector, Target, TicketSource, body_dependencies,
};
use crate::process;
use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use serde_json::Value;
use std::time::Duration;

/// Talks to GitHub with `gh api`.
#[derive(Clone, Debug)]
pub struct GitHub {
    program: String,
    timeout: Duration,
    /// How long to wait for a new fork, which GitHub creates in the
    /// background, and how often to look.
    fork_wait: Duration,
    fork_poll: Duration,
}

fn text(value: &Value) -> String {
    value.as_str().unwrap_or_default().to_owned()
}

fn names(list: &Value, key: &str) -> Vec<String> {
    list.as_array()
        .map(|items| items.iter().map(|i| text(&i[key])).collect())
        .unwrap_or_default()
}

fn to_issue(raw: &Value) -> Option<Issue> {
    if raw.get("pull_request").is_some() {
        return None;
    }
    Some(Issue {
        number: raw["number"].as_u64()?,
        key: None,
        title: text(&raw["title"]),
        body: text(&raw["body"]),
        labels: names(&raw["labels"], "name"),
        assignees: names(&raw["assignees"], "login"),
        milestone: raw["milestone"]["title"].as_str().map(str::to_owned),
        milestone_due: raw["milestone"]["due_on"].as_str().map(str::to_owned),
        open: raw["state"] == "open",
        created_at: text(&raw["created_at"]),
        blocked_by: Vec::new(),
    })
}

fn to_pr(raw: &Value) -> Result<PullRequest> {
    Ok(PullRequest {
        number: raw["number"]
            .as_u64()
            .context("pull request without number")?,
        url: text(&raw["html_url"]),
        open: raw["state"] == "open",
        merged: raw["merged_at"].is_string() || raw["merged"] == true,
        head_sha: text(&raw["head"]["sha"]),
    })
}

fn feedback(id: String, kind: FeedbackKind, raw: &Value) -> Feedback {
    Feedback {
        id,
        kind,
        author: text(&raw["user"]["login"]),
        body: text(&raw["body"]),
        path: raw["path"].as_str().map(str::to_owned),
        line: raw["line"].as_u64(),
        node_id: raw["node_id"].as_str().map(str::to_owned),
    }
}

/// Accept `repo` as the fork `fork` only when GitHub says it is a fork of
/// `upstream`, directly or through another fork.
fn check_fork(repo: &Value, fork: &str, upstream: &str) -> Result<()> {
    let is = |key: &str| {
        repo[key]["full_name"]
            .as_str()
            .is_some_and(|n| n.eq_ignore_ascii_case(upstream))
    };
    if repo["fork"] == true && (is("parent") || is("source")) {
        return Ok(());
    }
    let what = match repo["parent"]["full_name"].as_str() {
        Some(parent) => format!("a fork of {parent}"),
        None => "not a fork".to_owned(),
    };
    Err(ForkError(format!(
        "{fork} exists but is {what}, not a fork of {upstream}; delete or rename it, or set `fork` to the bot's fork of {upstream}"
    ))
    .into())
}

/// Percent-encode a path segment.
fn encode(segment: &str) -> String {
    segment
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
                char::from(b).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

fn items(value: &Value) -> &[Value] {
    value.as_array().map_or(&[], Vec::as_slice)
}

impl GitHub {
    pub const fn new(program: String) -> Self {
        Self {
            program,
            timeout: Duration::from_secs(120),
            fork_wait: Duration::from_secs(300),
            fork_poll: Duration::from_secs(5),
        }
    }

    #[must_use]
    pub const fn with_fork_wait(mut self, wait: Duration, poll: Duration) -> Self {
        self.fork_wait = wait;
        self.fork_poll = poll;
        self
    }

    /// A repository, or `None` when GitHub says it does not exist.
    async fn repository(&self, repo: &str) -> Result<Option<Value>> {
        match self.api(&[&format!("repos/{repo}")]).await {
            Ok(value) => Ok(Some(value)),
            Err(e) if format!("{e:#}").contains("HTTP 404") => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Fork `upstream` as `fork`: into the bot's account, or into the
    /// organization that owns `fork`.
    async fn create_fork(&self, upstream: &str, fork: &str) -> Result<()> {
        let (owner, name) = fork.split_once('/').context("fork is not owner/name")?;
        let me = self.api(&["user"]).await?;
        let login = text(&me["login"]);
        let path = format!("repos/{upstream}/forks");
        let name_field = format!("name={name}");
        let org_field = format!("organization={owner}");
        let mut args = vec!["-X", "POST", path.as_str(), "-f", name_field.as_str()];
        if !owner.eq_ignore_ascii_case(&login) {
            args.extend(["-f", org_field.as_str()]);
        }
        let created = self
            .api(&args)
            .await
            .with_context(|| format!("cannot create the fork {fork} of {upstream}"))?;
        // GitHub answers with the fork the account already has, under its
        // own name, instead of making a second one.
        let full_name = text(&created["full_name"]);
        if !full_name.eq_ignore_ascii_case(fork) {
            return Err(ForkError(format!(
                "GitHub made or kept the fork of {upstream} as {full_name}, not {fork}; set `fork: {full_name}` in the project file"
            ))
            .into());
        }
        Ok(())
    }

    /// Wait until GitHub serves a fork it is still creating.
    async fn wait_for_fork(&self, fork: &str) -> Result<Value> {
        let start = tokio::time::Instant::now();
        loop {
            if let Some(repo) = self.repository(fork).await? {
                return Ok(repo);
            }
            if start.elapsed() >= self.fork_wait {
                bail!(
                    "the fork {fork} was not ready after {}s",
                    self.fork_wait.as_secs()
                );
            }
            tokio::time::sleep(self.fork_poll).await;
        }
    }

    async fn api(&self, args: &[&str]) -> Result<Value> {
        let mut full = vec!["api"];
        full.extend_from_slice(args);
        let out =
            process::capture(&self.program, &full, &std::env::temp_dir(), self.timeout).await?;
        if out.trim().is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_str(&out).context("gh api returned invalid JSON")
    }

    async fn list_issues(&self, repo: &str, filter: &[&str]) -> Result<Vec<Value>> {
        let path = format!("repos/{repo}/issues");
        let mut args = vec![
            "-X",
            "GET",
            path.as_str(),
            "-f",
            "state=open",
            "-f",
            "per_page=100",
        ];
        args.extend_from_slice(filter);
        Ok(items(&self.api(&args).await?).to_vec())
    }

    /// Open issues this one is blocked by, from GitHub and from its body.
    async fn blockers(&self, repo: &str, issue: &Issue) -> Result<Vec<u64>> {
        let path = format!(
            "repos/{repo}/issues/{}/dependencies/blocked_by",
            issue.number
        );
        // Older GitHub servers have no dependency API; the body still counts.
        let linked = self.api(&[path.as_str()]).await.unwrap_or(Value::Null);
        let mut open: Vec<u64> = items(&linked)
            .iter()
            .filter(|i| i["state"] == "open")
            .filter_map(|i| i["number"].as_u64())
            .collect();
        for number in body_dependencies(&issue.body) {
            if !open.contains(&number) && self.raw_issue(repo, number).await?["state"] == "open" {
                open.push(number);
            }
        }
        Ok(open)
    }

    async fn raw_issue(&self, repo: &str, number: u64) -> Result<Value> {
        self.api(&[&format!("repos/{repo}/issues/{number}")]).await
    }

    async fn complete(&self, repo: &str, raw: Vec<Value>) -> Result<Vec<Issue>> {
        let mut issues = Vec::new();
        for mut issue in raw.iter().filter_map(to_issue) {
            if !issues.iter().any(|i: &Issue| i.number == issue.number) {
                issue.blocked_by = self.blockers(repo, &issue).await?;
                issues.push(issue);
            }
        }
        Ok(issues)
    }

    async fn milestone_number(&self, repo: &str, wanted: &str) -> Result<String> {
        let path = format!("repos/{repo}/milestones");
        let list = self
            .api(&["-X", "GET", &path, "-f", "state=open", "-f", "per_page=100"])
            .await?;
        let found = items(&list).iter().find(|m| {
            let title = m["title"].as_str().unwrap_or_default();
            title.eq_ignore_ascii_case(wanted) || wanted.parse::<u64>().ok() == m["number"].as_u64()
        });
        match found.and_then(|m| m["number"].as_u64()) {
            Some(number) => Ok(number.to_string()),
            None => bail!("no open milestone '{wanted}' in {repo}"),
        }
    }
}

#[async_trait]
impl TicketSource for GitHub {
    async fn eligible(&self, repo: &str, selector: &Selector) -> Result<Vec<Issue>> {
        let mut raw = Vec::new();
        for label in &selector.labels {
            raw.extend(
                self.list_issues(repo, &["-f", &format!("labels={label}")])
                    .await?,
            );
        }
        if let Some(login) = &selector.assignee {
            raw.extend(
                self.list_issues(repo, &["-f", &format!("assignee={login}")])
                    .await?,
            );
        }
        self.complete(repo, raw).await
    }

    async fn milestone(&self, repo: &str, milestone: &str) -> Result<Vec<Issue>> {
        let number = self.milestone_number(repo, milestone).await?;
        let raw = self
            .list_issues(repo, &["-f", &format!("milestone={number}")])
            .await?;
        self.complete(repo, raw).await
    }

    async fn issue(&self, repo: &str, number: u64) -> Result<Issue> {
        let raw = self.raw_issue(repo, number).await?;
        let mut issue =
            to_issue(&raw).with_context(|| format!("{repo}#{number} is not an issue"))?;
        issue.blocked_by = self.blockers(repo, &issue).await?;
        Ok(issue)
    }

    async fn comment(&self, repo: &str, number: u64, body: &str) -> Result<()> {
        let path = format!("repos/{repo}/issues/{number}/comments");
        self.api(&[&path, "-f", &format!("body={body}")]).await?;
        Ok(())
    }

    async fn add_label(&self, repo: &str, number: u64, label: &str) -> Result<()> {
        let path = format!("repos/{repo}/issues/{number}/labels");
        self.api(&[&path, "-f", &format!("labels[]={label}")])
            .await?;
        Ok(())
    }

    async fn remove_label(&self, repo: &str, number: u64, label: &str) -> Result<()> {
        let path = format!("repos/{repo}/issues/{number}/labels/{}", encode(label));
        // Removing a label that is not there is not an error for the daemon.
        let _ = self.api(&["-X", "DELETE", &path]).await;
        Ok(())
    }
}

#[async_trait]
impl Forge for GitHub {
    async fn ensure_fork(&self, target: &Target) -> Result<()> {
        let Some(fork) = &target.fork else {
            return Ok(());
        };
        let repo = match self.repository(fork).await? {
            Some(repo) => repo,
            None => {
                crate::scheduler::log(&format!("creating the fork {fork} of {}", target.upstream));
                self.create_fork(&target.upstream, fork).await?;
                self.wait_for_fork(fork).await?
            }
        };
        check_fork(&repo, fork, &target.upstream)
    }

    async fn find_pr(&self, target: &Target, branch: &str) -> Result<Option<PullRequest>> {
        let path = format!("repos/{}/pulls", target.upstream);
        let head = format!("head={}", target.head(branch));
        let list = self
            .api(&["-X", "GET", &path, "-f", &head, "-f", "state=all"])
            .await?;
        items(&list).first().map(to_pr).transpose()
    }

    async fn open_pr(&self, target: &Target, pr: &NewPullRequest) -> Result<PullRequest> {
        let path = format!("repos/{}/pulls", target.upstream);
        let fields = [
            format!("title={}", pr.title),
            format!("head={}", target.head(&pr.branch)),
            format!("base={}", pr.base),
            format!("body={}", pr.body),
        ];
        let mut args = vec![path.as_str()];
        for field in &fields {
            args.extend(["-f", field.as_str()]);
        }
        to_pr(&self.api(&args).await?)
    }

    async fn pr(&self, repo: &str, number: u64) -> Result<PullRequest> {
        to_pr(&self.api(&[&format!("repos/{repo}/pulls/{number}")]).await?)
    }

    async fn feedback(&self, repo: &str, pr: &PullRequest) -> Result<Vec<Feedback>> {
        let n = pr.number;
        let mut found = self.reviews(repo, n).await?;
        for c in self
            .list(&format!("repos/{repo}/pulls/{n}/comments"))
            .await?
        {
            found.push(feedback(
                format!("line-{}", c["id"]),
                FeedbackKind::LineComment,
                &c,
            ));
        }
        for c in self
            .list(&format!("repos/{repo}/issues/{n}/comments"))
            .await?
        {
            found.push(feedback(
                format!("comment-{}", c["id"]),
                FeedbackKind::Comment,
                &c,
            ));
        }
        found.extend(self.failed_checks(repo, &pr.head_sha).await?);
        Ok(found)
    }

    async fn reply(&self, repo: &str, pr: u64, to: &Feedback, body: &str) -> Result<()> {
        let body = format!("body={body}");
        if let (FeedbackKind::LineComment, Some(id)) = (to.kind, to.id.strip_prefix("line-")) {
            let path = format!("repos/{repo}/pulls/{pr}/comments/{id}/replies");
            self.api(&[&path, "-f", &body]).await?;
        } else {
            let path = format!("repos/{repo}/issues/{pr}/comments");
            self.api(&[&path, "-f", &body]).await?;
        }
        Ok(())
    }

    async fn react(&self, to: &Feedback, reaction: Reaction) -> Result<()> {
        let Some(content) = reaction.github_name() else {
            return Ok(());
        };
        let id = to
            .node_id
            .as_deref()
            .with_context(|| format!("{} has no node id to react to", to.id))?;
        // GraphQL, because reviews have no REST endpoint for reactions.
        let query = format!(
            "query=mutation($id: ID!) {{ addReaction(input: {{subjectId: $id, content: {content}}}) {{ reaction {{ content }} }} }}"
        );
        self.api(&["graphql", "-f", &query, "-f", &format!("id={id}")])
            .await?;
        Ok(())
    }
}

impl GitHub {
    async fn list(&self, path: &str) -> Result<Vec<Value>> {
        let value = self.api(&[&format!("{path}?per_page=100")]).await?;
        Ok(items(&value).to_vec())
    }

    /// Reviews that carry feedback: a body, or "changes requested".
    async fn reviews(&self, repo: &str, pr: u64) -> Result<Vec<Feedback>> {
        let reviews = self
            .list(&format!("repos/{repo}/pulls/{pr}/reviews"))
            .await?;
        Ok(reviews
            .iter()
            .filter(|r| r["state"] == "CHANGES_REQUESTED" || !text(&r["body"]).trim().is_empty())
            .map(|r| feedback(format!("review-{}", r["id"]), FeedbackKind::Review, r))
            .collect())
    }

    async fn failed_checks(&self, repo: &str, sha: &str) -> Result<Vec<Feedback>> {
        if sha.is_empty() {
            return Ok(Vec::new());
        }
        let runs = self
            .api(&[&format!(
                "repos/{repo}/commits/{sha}/check-runs?per_page=100"
            )])
            .await?;
        Ok(items(&runs["check_runs"])
            .iter()
            .filter(|r| matches!(r["conclusion"].as_str(), Some("failure" | "timed_out")))
            .map(|r| Feedback {
                id: format!("check-{}-{sha}", r["id"]),
                kind: FeedbackKind::FailedCheck,
                author: "ci".into(),
                body: format!(
                    "Check '{}' failed: {}\n{}",
                    text(&r["name"]),
                    text(&r["output"]["title"]),
                    text(&r["output"]["summary"])
                ),
                path: None,
                line: None,
                node_id: None,
            })
            .collect())
    }
}

#[cfg(test)]
mod tests;
