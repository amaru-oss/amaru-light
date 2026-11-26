//! Where tickets come from and where pull requests go.
//!
//! `TicketSource` and `Forge` are separate so a ticket from one system (for
//! example Jira) can still end as a pull request on another (GitHub).

pub mod github;
pub mod jira;

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// An issue as the daemon needs it.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct Issue {
    pub number: u64,
    /// The Jira key, such as `CAMEL-12345`, for an issue from Jira.
    #[serde(default)]
    pub key: Option<String>,
    pub title: String,
    pub body: String,
    pub labels: Vec<String>,
    pub assignees: Vec<String>,
    pub milestone: Option<String>,
    /// The milestone's due date, as an RFC 3339 time.
    pub milestone_due: Option<String>,
    pub open: bool,
    pub created_at: String,
    /// Open issues this one depends on.
    pub blocked_by: Vec<u64>,
}

/// Which issues the autonomous mode picks up: any label, or the assignee.
#[derive(Clone, Debug, Default)]
pub struct Selector {
    pub labels: Vec<String>,
    pub assignee: Option<String>,
}

/// Where branches are pushed and pull requests are opened.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    /// The repository the pull request is opened on.
    pub upstream: String,
    /// Set for fork-based contributions: the repository branches go to.
    pub fork: Option<String>,
}

impl Target {
    /// The repository that receives the pushed branches.
    pub fn push_repo(&self) -> &str {
        self.fork.as_deref().unwrap_or(&self.upstream)
    }

    /// The `head` of a pull request: `owner:branch` for forks.
    pub fn head(&self, branch: &str) -> String {
        let owner = self.push_repo().split('/').next().unwrap_or_default();
        format!("{owner}:{branch}")
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct PullRequest {
    pub number: u64,
    pub url: String,
    pub open: bool,
    pub merged: bool,
    pub head_sha: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackKind {
    /// A submitted review with a body or "changes requested".
    Review,
    /// A comment on a line of the diff.
    LineComment,
    /// A comment on the pull request conversation.
    Comment,
    /// A failing check run.
    FailedCheck,
}

/// One piece of feedback on a pull request.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Feedback {
    /// Stable across polls, used to remember what was handled.
    pub id: String,
    pub kind: FeedbackKind,
    pub author: String,
    pub body: String,
    pub path: Option<String>,
    pub line: Option<u64>,
    /// GitHub's global id of the review or comment, used to react to it.
    #[serde(default)]
    pub node_id: Option<String>,
}

/// The emoji the daemon puts on feedback it picks up.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Reaction {
    /// React to nothing.
    Off,
    #[default]
    ThumbsUp,
    Eyes,
    Heart,
    Rocket,
    Hooray,
    Laugh,
}

impl Reaction {
    /// The name GitHub's GraphQL API uses.
    pub const fn github_name(self) -> Option<&'static str> {
        match self {
            Self::Off => None,
            Self::ThumbsUp => Some("THUMBS_UP"),
            Self::Eyes => Some("EYES"),
            Self::Heart => Some("HEART"),
            Self::Rocket => Some("ROCKET"),
            Self::Hooray => Some("HOORAY"),
            Self::Laugh => Some("LAUGH"),
        }
    }
}

#[async_trait]
pub trait TicketSource: Send + Sync {
    /// Open issues matching the selector.
    async fn eligible(&self, repo: &str, selector: &Selector) -> Result<Vec<Issue>>;
    /// Open issues of a milestone, found by title or number.
    async fn milestone(&self, repo: &str, milestone: &str) -> Result<Vec<Issue>>;
    async fn issue(&self, repo: &str, number: u64) -> Result<Issue>;
    async fn comment(&self, repo: &str, number: u64, body: &str) -> Result<()>;
    async fn add_label(&self, repo: &str, number: u64, label: &str) -> Result<()>;
    async fn remove_label(&self, repo: &str, number: u64, label: &str) -> Result<()>;
}

/// The fork of a target cannot be used, and retrying will not change that:
/// for example the repository exists but is not a fork of the upstream one.
#[derive(Debug)]
pub struct ForkError(pub String);

impl std::fmt::Display for ForkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ForkError {}

#[async_trait]
pub trait Forge: Send + Sync {
    /// Make sure the target's fork exists and is a fork of its upstream
    /// repository, creating it when it is missing. Does nothing without a
    /// fork. Safe to call before every step.
    async fn ensure_fork(&self, target: &Target) -> Result<()>;
    /// The pull request for a branch, open or not.
    async fn find_pr(&self, target: &Target, branch: &str) -> Result<Option<PullRequest>>;
    async fn open_pr(&self, target: &Target, pr: &NewPullRequest) -> Result<PullRequest>;
    async fn pr(&self, repo: &str, number: u64) -> Result<PullRequest>;
    /// Reviews, comments and failing checks on the pull request.
    async fn feedback(&self, repo: &str, pr: &PullRequest) -> Result<Vec<Feedback>>;
    /// Answer feedback: in its thread for line comments, otherwise on the PR.
    async fn reply(&self, repo: &str, pr: u64, to: &Feedback, body: &str) -> Result<()>;
    /// Put an emoji on a review or comment. Reacting twice with the same
    /// emoji leaves one reaction.
    async fn react(&self, to: &Feedback, reaction: Reaction) -> Result<()>;
}

/// Sends each project's ticket calls to its tracker: Jira for the
/// repositories that have one, GitHub for every other.
#[derive(Clone)]
pub struct Trackers {
    default: Arc<dyn TicketSource>,
    /// Lowercase `owner/name` and the tracker for it.
    by_repo: Vec<(String, Arc<dyn TicketSource>)>,
}

impl std::fmt::Debug for Trackers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let repos: Vec<&str> = self.by_repo.iter().map(|(r, _)| r.as_str()).collect();
        f.debug_struct("Trackers")
            .field("by_repo", &repos)
            .finish_non_exhaustive()
    }
}

impl Trackers {
    pub fn new(default: Arc<dyn TicketSource>) -> Self {
        Self {
            default,
            by_repo: Vec::new(),
        }
    }

    /// Use `source` for the tickets of `repo`.
    #[must_use]
    pub fn with(mut self, repo: &str, source: Arc<dyn TicketSource>) -> Self {
        self.by_repo.push((repo.to_lowercase(), source));
        self
    }

    fn of(&self, repo: &str) -> &dyn TicketSource {
        let repo = repo.to_lowercase();
        self.by_repo
            .iter()
            .find(|(r, _)| *r == repo)
            .map_or(self.default.as_ref(), |(_, s)| s.as_ref())
    }
}

#[async_trait]
impl TicketSource for Trackers {
    async fn eligible(&self, repo: &str, selector: &Selector) -> Result<Vec<Issue>> {
        self.of(repo).eligible(repo, selector).await
    }
    async fn milestone(&self, repo: &str, milestone: &str) -> Result<Vec<Issue>> {
        self.of(repo).milestone(repo, milestone).await
    }
    async fn issue(&self, repo: &str, number: u64) -> Result<Issue> {
        self.of(repo).issue(repo, number).await
    }
    async fn comment(&self, repo: &str, number: u64, body: &str) -> Result<()> {
        self.of(repo).comment(repo, number, body).await
    }
    async fn add_label(&self, repo: &str, number: u64, label: &str) -> Result<()> {
        self.of(repo).add_label(repo, number, label).await
    }
    async fn remove_label(&self, repo: &str, number: u64, label: &str) -> Result<()> {
        self.of(repo).remove_label(repo, number, label).await
    }
}

/// What a new pull request contains.
#[derive(Clone, Debug)]
pub struct NewPullRequest {
    pub branch: String,
    pub base: String,
    pub title: String,
    pub body: String,
}

/// Find "Depends on #12" or "Blocked by #12" in an issue body.
pub fn body_dependencies(body: &str) -> Vec<u64> {
    let lower = body.to_lowercase();
    let mut found = Vec::new();
    for marker in ["depends on #", "blocked by #"] {
        for (at, _) in lower.match_indices(marker) {
            let digits: String = lower[at + marker.len()..]
                .chars()
                .take_while(char::is_ascii_digit)
                .collect();
            if let Ok(n) = digits.parse()
                && !found.contains(&n)
            {
                found.push(n);
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fork_targets_push_to_the_fork() {
        let direct = Target {
            upstream: "org/app".into(),
            fork: None,
        };
        assert_eq!(direct.push_repo(), "org/app");
        assert_eq!(direct.head("amaru/1-x"), "org:amaru/1-x");
        let fork = Target {
            upstream: "org/app".into(),
            fork: Some("bot/app".into()),
        };
        assert_eq!(fork.push_repo(), "bot/app");
        assert_eq!(fork.head("amaru/1-x"), "bot:amaru/1-x");
    }

    #[test]
    fn finds_dependencies_in_the_body() {
        let body = "Depends on #12.\nAlso blocked by #7 and depends on #12";
        assert_eq!(body_dependencies(body), [12, 7]);
        assert!(body_dependencies("see #3").is_empty());
    }
}
