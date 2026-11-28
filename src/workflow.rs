//! Workflows: which steps a ticket goes through, as data.
//!
//! The step kinds are a closed set written in Rust; a workflow is an ordered
//! list of them, named in the daemon file and chosen per project. `code`, the
//! built-in workflow, is the path from an issue to a reviewed pull request.

use crate::ticket::State;
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use std::fmt;

/// The name of the built-in workflow.
pub const CODE: &str = "code";

/// One kind of step a workflow can contain.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Step {
    /// The agent implements the issue and commits on the ticket's branch.
    Implement,
    /// The project checks run; failures go back to the agent.
    Check,
    /// An independent session reviews the change, when `review.self` is on.
    SelfReview,
    /// The branch is pushed and the pull request opened.
    Publish,
    /// Review feedback on the pull request is addressed until it is merged.
    ReviewLoop,
}

impl Step {
    /// Every kind, in the only order a workflow may list them.
    pub const ALL: [Self; 5] = [
        Self::Implement,
        Self::Check,
        Self::SelfReview,
        Self::Publish,
        Self::ReviewLoop,
    ];

    /// The state of a ticket when this step is next.
    pub const fn state(self) -> State {
        match self {
            Self::Implement => State::Implementing,
            Self::Check => State::Checking,
            Self::SelfReview => State::SelfReview,
            Self::Publish => State::Publishing,
            Self::ReviewLoop => State::AwaitingReview,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Implement => "implement",
            Self::Check => "check",
            Self::SelfReview => "self_review",
            Self::Publish => "publish",
            Self::ReviewLoop => "review_loop",
        }
    }
}

impl fmt::Display for Step {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A named, validated list of steps.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Workflow {
    pub name: String,
    pub steps: Vec<Step>,
}

impl Workflow {
    /// The built-in workflow: implement, check, self-review, publish, then
    /// follow the reviews until a human merges.
    pub fn code() -> Self {
        Self {
            name: CODE.into(),
            steps: Step::ALL.to_vec(),
        }
    }

    /// Check that the steps make sense together.
    pub fn new(name: &str, steps: Vec<Step>) -> Result<Self> {
        if steps.is_empty() {
            bail!("workflow '{name}' has no steps");
        }
        if !steps.windows(2).all(|w| w[0] < w[1]) {
            let order: Vec<&str> = Step::ALL.iter().map(|s| s.as_str()).collect();
            bail!(
                "workflow '{name}' must list each step once, in this order: {}",
                order.join(", ")
            );
        }
        let has = |step| steps.contains(&step);
        if !has(Step::Implement) {
            bail!("workflow '{name}' needs an implement step");
        }
        if !has(Step::Publish) {
            // Without it the work would stay in a worktree nobody sees.
            bail!("workflow '{name}' needs a publish step after implement");
        }
        Ok(Self {
            name: name.to_owned(),
            steps,
        })
    }

    pub fn has(&self, step: Step) -> bool {
        self.steps.contains(&step)
    }

    /// The state of a ticket whose worktree is ready.
    pub fn first(&self) -> State {
        self.steps.first().map_or(State::Done, |s| s.state())
    }

    /// The state after `step`: the next step of this workflow that `applies`,
    /// or done. `step` does not have to be part of the workflow; the review
    /// loop, for example, runs the checks whether or not they are a step.
    pub fn after(&self, step: Step, applies: impl Fn(Step) -> bool) -> State {
        self.steps
            .iter()
            .copied()
            .find(|s| *s > step && applies(*s))
            .map_or(State::Done, Step::state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all(_: Step) -> bool {
        true
    }

    #[test]
    fn the_code_workflow_runs_every_step() {
        let code = Workflow::code();
        assert_eq!(code.first(), State::Implementing);
        assert_eq!(code.after(Step::Implement, all), State::Checking);
        assert_eq!(code.after(Step::Check, all), State::SelfReview);
        let no_self_review = |s| s != Step::SelfReview;
        assert_eq!(code.after(Step::Check, no_self_review), State::Publishing);
        assert_eq!(code.after(Step::Publish, all), State::AwaitingReview);
        assert_eq!(code.after(Step::ReviewLoop, all), State::Done);
    }

    #[test]
    fn skips_steps_a_workflow_leaves_out() {
        let quick = Workflow::new("quick", vec![Step::Implement, Step::Publish]).unwrap();
        assert_eq!(quick.after(Step::Implement, all), State::Publishing);
        assert_eq!(quick.after(Step::Check, all), State::Publishing);
        assert_eq!(quick.after(Step::Publish, all), State::Done);
        assert!(!quick.has(Step::Check));
    }

    #[test]
    fn rejects_steps_that_do_not_fit_together() {
        use Step::*;
        for (steps, error) in [
            (vec![], "no steps"),
            (vec![Implement, Publish, Check], "in this order"),
            (vec![Implement, Implement, Publish], "in this order"),
            (vec![Check, Publish], "needs an implement step"),
            (vec![Implement, Check], "needs a publish step"),
        ] {
            let err = Workflow::new("w", steps).unwrap_err();
            assert!(err.to_string().contains(error), "{err}");
        }
    }

    #[test]
    fn steps_read_from_yaml() {
        let steps: Vec<Step> = serde_yaml::from_str("[implement, check, publish]").unwrap();
        assert_eq!(steps, [Step::Implement, Step::Check, Step::Publish]);
        assert!(serde_yaml::from_str::<Vec<Step>>("[deploy]").is_err());
    }
}
