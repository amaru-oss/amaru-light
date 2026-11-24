//! Tickets and their lifecycle states.

use serde::{Deserialize, Serialize};
use std::{fmt, path::PathBuf};

/// Where a ticket is in its lifecycle.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum State {
    Queued,
    Preparing,
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
    pub const ALL: [Self; 11] = [
        Self::Queued,
        Self::Preparing,
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

/// How a ticket entered the queue.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    Manual,
    Milestone,
    Autonomous,
}

/// One GitHub issue being worked on.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Ticket {
    pub id: i64,
    pub repo: String,
    pub number: u64,
    pub title: String,
    pub origin: Origin,
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
    /// When the ticket became blocked, as an RFC 3339 time.
    pub blocked_at: Option<String>,
}

impl Ticket {
    pub const fn new(id: i64, repo: String, number: u64, title: String, origin: Origin) -> Self {
        Self {
            id,
            repo,
            number,
            title,
            origin,
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
            blocked_at: None,
        }
    }

    /// `owner/name#42`, for logs and comments.
    pub fn reference(&self) -> String {
        format!("{}#{}", self.repo, self.number)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
