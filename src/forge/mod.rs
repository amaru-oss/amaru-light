//! Where tickets come from and where pull requests go.
//!
//! `TicketSource` and `Forge` are separate so a ticket from one system (for
//! example Jira) can still end as a pull request on another (GitHub).

pub mod github;

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// An issue as the daemon needs it.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct Issue {
    pub number: u64,
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

#[async_trait]
pub trait Forge: Send + Sync {
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
