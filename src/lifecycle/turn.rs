//! One agent turn: run, keep the session, and read the result file.

use super::{Cancelled, Job, blocked};
use crate::{
    agent::{Agent, AgentEvent, Turn, TurnResult, Workplace},
    contract::{self, Report},
    process::Exit,
    process::tail,
};
use anyhow::Result;
use serde_json::json;

/// Whose session a turn runs in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Session {
    /// The ticket's own session, resumed and kept.
    Ticket,
    /// A separate session, for example an independent reviewer.
    Separate,
}

fn event_payload(event: &AgentEvent) -> Option<(&'static str, serde_json::Value)> {
    match event {
        AgentEvent::Session(id) => Some(("AGENT_SESSION", json!({ "session": id }))),
        AgentEvent::Tool(name) => Some(("AGENT_TOOL", json!({ "tool": name }))),
        AgentEvent::Text(text) => Some(("AGENT_TEXT", json!({ "text": tail(text, 500) }))),
        AgentEvent::Finished => None,
    }
}

impl Job<'_> {
    async fn run_agent(
        &self,
        agent: &str,
        prompt: &str,
        session: Option<&str>,
    ) -> Result<TurnResult> {
        let agent = Agent::new(self.svc.settings.agent(agent)?.clone());
        let cwd = self.worktree()?;
        let place = Workplace {
            cwd: &cwd,
            timeout: self.project.step_timeout.0,
            cancel: self.cancel.clone(),
        };
        let (store, id) = (&self.svc.store, self.ticket.id);
        let result = agent
            .run(&Turn { prompt, session }, place, &mut |event| {
                if let Some((kind, payload)) = event_payload(event) {
                    // The event log is best effort; the turn result is what counts.
                    let _ = store.event(id, kind, &payload);
                }
            })
            .await?;
        self.event(
            "AGENT_TURN",
            &json!({ "exit": format!("{:?}", result.exit), "error": result.is_error,
                     "cost_usd": result.cost_usd, "turns": result.turns }),
        )?;
        Ok(result)
    }

    /// The prompt that rebuilds context when a session cannot be resumed.
    async fn context_pack(&self, prompt: &str) -> Result<String> {
        let (log, stat) = self
            .svc
            .workspace
            .summary(&self.worktree()?, &self.base_ref()?)
            .await?;
        let mut vars = self.issue_vars();
        vars.extend([("log".to_owned(), log), ("stat".to_owned(), stat)]);
        Ok(format!(
            "{}{prompt}",
            self.svc.prompts.text("context", &as_refs(&vars))
        ))
    }

    async fn attempt(&mut self, agent: &str, prompt: &str, mode: Session) -> Result<TurnResult> {
        let session = match mode {
            Session::Ticket => self.ticket.session_id.clone(),
            Session::Separate => None,
        };
        let mut result = self.run_agent(agent, prompt, session.as_deref()).await?;
        let lost_session =
            session.is_some() && !result.succeeded() && result.exit == Some(Exit::Code(1));
        if lost_session {
            self.event("SESSION_LOST", &json!({ "stderr": result.stderr }))?;
            let fresh = self.context_pack(prompt).await?;
            result = self.run_agent(agent, &fresh, None).await?;
        }
        self.judge(&result)?;
        if mode == Session::Ticket && result.session_id.is_some() {
            self.ticket.session_id.clone_from(&result.session_id);
            self.save()?;
        }
        Ok(result)
    }

    /// Turn a failed run into the right stop.
    fn judge(&self, result: &TurnResult) -> Result<()> {
        match result.exit {
            Some(Exit::Cancelled) => Err(Cancelled.into()),
            Some(Exit::TimedOut) => Err(blocked(format!(
                "the agent did not finish within {}",
                self.project.step_timeout
            ))),
            _ if !result.succeeded() => {
                let detail = if result.text.is_empty() {
                    &result.stderr
                } else {
                    &result.text
                };
                Err(blocked(format!(
                    "the agent failed: {}",
                    tail(detail, 1_000)
                )))
            }
            _ => Ok(()),
        }
    }

    /// Run a turn and return the validated result file.
    pub(super) async fn turn(
        &mut self,
        agent: &str,
        prompt: &str,
        mode: Session,
    ) -> Result<Report> {
        let worktree = self.worktree()?;
        contract::clear(&worktree)?;
        let first = self.attempt(agent, prompt, mode).await?;
        match contract::read(&worktree) {
            Ok(report) => Ok(report),
            Err(error) => {
                let ask = self
                    .svc
                    .prompts
                    .render("missing_result", &[("error", &format!("{error:#}"))]);
                let session = first.session_id.clone();
                let retry = self.run_agent(agent, &ask, session.as_deref()).await?;
                if !retry.succeeded() {
                    return Err(blocked(format!(
                        "the agent did not report its result: {error:#}"
                    )));
                }
                contract::read(&worktree).map_err(|e| blocked(format!("{e:#}")))
            }
        }
    }

    /// Placeholders that describe the ticket's issue and branch.
    pub(super) fn issue_vars(&self) -> Vec<(String, String)> {
        let t = &self.ticket;
        vec![
            ("repo".into(), t.repo.clone()),
            ("number".into(), t.number.to_string()),
            ("title".into(), t.title.clone()),
            ("body".into(), t.body.clone()),
            ("branch".into(), t.branch.clone().unwrap_or_default()),
            ("base".into(), t.base_ref().unwrap_or_default()),
        ]
    }
}

/// Borrow owned placeholder pairs for `Prompts`.
pub(super) fn as_refs(vars: &[(String, String)]) -> Vec<(&str, &str)> {
    vars.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect()
}
