//! Tickets and their lifecycle states.

use serde::{Deserialize, Serialize};
use std::{fmt, path::PathBuf};

/// Where a ticket is in its lifecycle.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum State {
    Queued,
    Preparing,
    /// Analyzing the issue and commenting on it, instead of implementing.
    Analyzing,
    Implementing,
    Checking,
    SelfReview,
    Publishing,
    AwaitingReview,
    AddressingReview,
    Done,
    Blocked,
    Cancelled,
}

impl State {
    pub const ALL: [Self; 12] = [
        Self::Queued,
        Self::Preparing,
        Self::Analyzing,
        Self::Implementing,
        Self::Checking,
        Self::SelfReview,
        Self::Publishing,
        Self::AwaitingReview,
        Self::AddressingReview,
        Self::Done,
        Self::Blocked,
        Self::Cancelled,
    ];

    /// No further work will happen without a human.
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Done | Self::Cancelled)
    }

    /// The ticket needs a worker slot to make progress.
    pub const fn needs_slot(self) -> bool {
        matches!(
            self,
            Self::Queued
                | Self::Preparing
                | Self::Analyzing
                | Self::Implementing
                | Self::Checking
                | Self::SelfReview
                | Self::Publishing
                | Self::AddressingReview
        )
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "QUEUED",
            Self::Preparing => "PREPARING",
            Self::Analyzing => "ANALYZING",
            Self::Implementing => "IMPLEMENTING",
            Self::Checking => "CHECKING",
            Self::SelfReview => "SELF_REVIEW",
            Self::Publishing => "PUBLISHING",
            Self::AwaitingReview => "AWAITING_REVIEW",
            Self::AddressingReview => "ADDRESSING_REVIEW",
            Self::Done => "DONE",
            Self::Blocked => "BLOCKED",
            Self::Cancelled => "CANCELLED",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.as_str() == text)
    }
}

impl fmt::Display for State {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What a ticket does with its issue.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Goal {
    /// Implement it and open a pull request.
    #[default]
    Implement,
    /// Analyze it and post the analysis as a comment.
    Analyze,
}

/// How a ticket entered the queue.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    Manual,
    Milestone,
    Autonomous,
}

/// One issue being worked on: a GitHub issue of `repo`, or a Jira issue of
/// the project that `repo` belongs to.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Ticket {
    pub id: i64,
    pub repo: String,
    pub number: u64,
    /// The Jira key, such as `CAMEL-12345`, when the issue lives in Jira.
    #[serde(default)]
    pub key: Option<String>,
    pub title: String,
    /// The issue body when the ticket was prepared.
    #[serde(default)]
    pub body: String,
    pub origin: Origin,
    /// What the ticket does with its issue.
    #[serde(default)]
    pub goal: Goal,
    pub state: State,
    /// Why the ticket is blocked, or what it waits for.
    pub reason: Option<String>,
    pub branch: Option<String>,
    /// The branch this ticket's branch starts from and targets.
    pub base: Option<String>,
    /// Another ticket's issue number when this branch is stacked on it.
    pub stacked_on: Option<u64>,
    pub worktree: Option<PathBuf>,
    /// The agent CLI session, resumed for every later step.
    pub session_id: Option<String>,
    pub pr_number: Option<u64>,
    pub fix_rounds: u32,
    pub review_rounds: u32,
    /// Feedback handled so far: review and comment ids.
    pub handled_feedback: Vec<String>,
    /// Feedback the daemon reacted to, or tried to, so it reacts only once.
    #[serde(default)]
    pub reacted_feedback: Vec<String>,
    /// When the ticket became blocked, as an RFC 3339 time.
    pub blocked_at: Option<String>,
    /// The state to return to when a human unblocks the ticket.
    #[serde(default)]
    pub blocked_from: Option<State>,
    /// The agent's latest summary of its work.
    #[serde(default)]
    pub summary: Option<String>,
    /// Whether the agent claims every requirement of the issue is met.
    #[serde(default)]
    pub complete: bool,
    #[serde(default)]
    pub self_reviewed: bool,
    /// Labels the daemon managed to add to the issue. Removing the blocked
    /// label resumes the ticket only when it is listed here.
    #[serde(default)]
    pub labels: Vec<String>,
}

impl Ticket {
    pub const fn new(id: i64, repo: String, number: u64, title: String, origin: Origin) -> Self {
        Self {
            id,
            repo,
            number,
            key: None,
            title,
            body: String::new(),
            origin,
            goal: Goal::Implement,
            state: State::Queued,
            reason: None,
            branch: None,
            base: None,
            stacked_on: None,
            worktree: None,
            session_id: None,
            pr_number: None,
            fix_rounds: 0,
            review_rounds: 0,
            handled_feedback: Vec::new(),
            reacted_feedback: Vec::new(),
            blocked_at: None,
            blocked_from: None,
            summary: None,
            complete: false,
            self_reviewed: false,
            labels: Vec::new(),
        }
    }

    /// The git ref the ticket branch starts from and is compared with.
    pub fn base_ref(&self) -> Option<String> {
        let base = self.base.as_deref()?;
        Some(if self.stacked_on.is_some() {
            base.to_owned()
        } else {
            format!("upstream/{base}")
        })
    }

    /// `owner/name#42` or the Jira key, for logs and comments.
    pub fn reference(&self) -> String {
        self.key
            .clone()
            .unwrap_or_else(|| format!("{}#{}", self.repo, self.number))
    }

    /// What branch names and pull request titles start with: the issue
    /// number, or the Jira key.
    pub fn label(&self) -> String {
        self.key.clone().unwrap_or_else(|| self.number.to_string())
    }
}

/// A branch-friendly form of an issue title.
pub fn slug(title: &str) -> String {
    let words: Vec<String> = title
        .to_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_owned)
        .collect();
    let mut slug = String::new();
    for word in words {
        if slug.len() + word.len() > 40 {
            break;
        }
        if !slug.is_empty() {
            slug.push('-');
        }
        slug.push_str(&word);
    }
    if slug.is_empty() { "work".into() } else { slug }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_and_base_refs() {
        assert_eq!(
            slug("Fix: the *Safari* export button!"),
            "fix-the-safari-export-button"
        );
        assert_eq!(slug("???"), "work");
        let mut t = Ticket::new(1, "me/a".into(), 1, "t".into(), Origin::Manual);
        t.base = Some("main".into());
        assert_eq!(t.base_ref().as_deref(), Some("upstream/main"));
        t.stacked_on = Some(3);
        t.base = Some("amaru/3-x".into());
        assert_eq!(t.base_ref().as_deref(), Some("amaru/3-x"));
    }

    #[test]
    fn states_round_trip() {
        for state in State::ALL {
            assert_eq!(State::parse(state.as_str()), Some(state));
            let json = serde_json::to_string(&state).unwrap();
            assert_eq!(json, format!("\"{state}\""));
        }
    }

    #[test]
    fn awaiting_review_frees_the_slot() {
        assert!(!State::AwaitingReview.needs_slot());
        assert!(!State::Blocked.needs_slot());
        assert!(State::Implementing.needs_slot());
        assert!(State::Done.is_terminal());
        assert!(!State::Blocked.is_terminal());
    }
}
